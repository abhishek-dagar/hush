//! Transitions and `@keyframes` animations (spec 0010, child 0005).
//!
//! [`AnimationSet`] holds every running transition and animation as plain
//! data, in document time (seconds, from `BeginFrame.frame_time`). Each
//! frame the page samples it, writes the sampled values over each
//! element's cascaded style before boxes are built, starts and stops
//! entries by comparing each element's style before and after the style
//! pass, and dispatches the queued events to script.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use base::Au;
use base::atoms::Atom;
use css::{
    AnimationDirection, AnimationSpec, ComputedStyle, Display, FillMode, KeyframesRule, PlayState,
    PropertyId, Stylist, TimingFunction, TransitionSpec,
};
use display_list::{Binding, BindingId};
use dom::{Document, NodeId};

/// How many transitions and animations one page may run at once; past it
/// new ones are ignored, with one `warn` (spec 0010, AC-38).
pub const MAX_RUNNING: usize = 10_000;

/// A running transition of one property (CSS Transitions 1, section 3).
#[derive(Debug, Clone)]
struct RunningTransition {
    /// A style holding the start value.
    from: Rc<ComputedStyle>,
    /// A style holding the end value.
    to: Rc<ComputedStyle>,
    /// The value a change back to reverses toward, for the reversing rule.
    reversing_start: Rc<ComputedStyle>,
    /// How much a reversal shortened this transition, 0 to 1.
    shortening: f64,
    start: f64,
    delay: f64,
    duration: f64,
    timing: TimingFunction,
    /// Whether `transitionstart` has fired.
    started: bool,
    /// The display list binding, stable for its life (spec 0010, AC-28).
    binding_id: BindingId,
}

impl RunningTransition {
    /// Seconds into the active interval at `now`, below 0 in the delay.
    fn elapsed(&self, now: f64) -> f64 {
        now - self.start - self.delay
    }

    /// The eased progress at `now`: the start value during the delay.
    fn progress(&self, now: f64) -> f32 {
        let elapsed = self.elapsed(now);
        let linear = if self.duration <= 0.0 {
            if elapsed >= 0.0 { 1.0 } else { 0.0 }
        } else {
            (elapsed / self.duration).clamp(0.0, 1.0)
        };
        ease(self.timing, linear as f32)
    }
}

/// A running `@keyframes` animation (CSS Animations 1).
#[derive(Debug, Clone)]
struct RunningAnimation {
    /// Its place in the element's `animation-name` list: later names win
    /// per property (spec 0010, AC-24).
    order: usize,
    /// Per animated property, its keyframes in offset order. Empty when no
    /// `@keyframes` rule matched: the animation runs with no effect and
    /// fires no events.
    tracks: Tracks,
    has_rule: bool,
    /// The element's own style, which missing end keyframes take.
    base: Rc<ComputedStyle>,
    /// Document time the animation started at.
    start: f64,
    duration: f64,
    delay: f64,
    /// `None` is `infinite`.
    iteration_count: Option<f64>,
    timing: TimingFunction,
    direction: AnimationDirection,
    fill_mode: FillMode,
    /// Document time it was paused at, while paused.
    paused_at: Option<f64>,
    started: bool,
    /// The iteration last sampled.
    iteration: u64,
    /// It reached its end. It stays in the set so it does not start again
    /// while its name is in the style, and keeps its end value only under
    /// `forwards` or `both`.
    finished: bool,
    binding_id: BindingId,
}

/// Where an animation is in its timeline at one time.
enum Phase {
    /// In the delay.
    Before,
    /// Playing: the iteration index and the progress in it, 0 to 1.
    Active(u64, f64),
    /// Past its end: the last iteration and where it stopped in it.
    After(u64, f64),
}

impl RunningAnimation {
    fn active_duration(&self) -> f64 {
        match self.iteration_count {
            Some(count) => self.duration * count.max(0.0),
            None => f64::INFINITY,
        }
    }

    /// Seconds past the delay at `now`, frozen while paused.
    fn elapsed(&self, now: f64) -> f64 {
        self.paused_at.unwrap_or(now) - self.start - self.delay
    }

    fn phase(&self, now: f64) -> Phase {
        let elapsed = self.elapsed(now);
        if elapsed < 0.0 {
            return Phase::Before;
        }
        let active = self.active_duration();
        if elapsed >= active {
            // The end of the last iteration, or partway into it for a
            // fractional count (CSS Animations 1, section 4.4).
            let count = self.iteration_count.unwrap_or(0.0).max(0.0);
            let whole = count.floor();
            return if count == 0.0 {
                Phase::After(0, 0.0)
            } else if count == whole {
                Phase::After(whole as u64 - 1, 1.0)
            } else {
                Phase::After(whole as u64, count - whole)
            };
        }
        if self.duration <= 0.0 {
            return Phase::Active(0, 1.0);
        }
        let iterations = elapsed / self.duration;
        Phase::Active(iterations.floor() as u64, iterations.fract())
    }

    /// Progress in iteration `iteration` turned by `animation-direction`.
    fn directed(&self, iteration: u64, progress: f64) -> f32 {
        let forward = match self.direction {
            AnimationDirection::Normal => true,
            AnimationDirection::Reverse => false,
            AnimationDirection::Alternate => iteration.is_multiple_of(2),
            AnimationDirection::AlternateReverse => !iteration.is_multiple_of(2),
        };
        (if forward { progress } else { 1.0 - progress }) as f32
    }
}

/// One sampled value: write `property` the eased `progress` of the way
/// from `from`'s value to `to`'s (spec 0010, AC-26).
#[derive(Debug, Clone)]
pub struct AnimatedValue {
    /// The element.
    pub node: NodeId,
    /// The property.
    pub property: PropertyId,
    /// A style holding the value at progress 0.
    pub from: Rc<ComputedStyle>,
    /// A style holding the value at progress 1.
    pub to: Rc<ComputedStyle>,
    /// How far along, eased; may leave 0 to 1 only by easing overshoot.
    pub progress: f32,
    /// The stable binding id of the animation or transition (spec 0010,
    /// AC-28).
    pub binding_id: BindingId,
}

impl AnimatedValue {
    /// Writes this value into `style`.
    pub fn apply(&self, style: &mut ComputedStyle) {
        self.property
            .interpolate(&self.from, &self.to, self.progress, style);
    }
}

/// The sampled values of one frame, animations first in `animation-name`
/// order and transitions last, since transition values win (spec 0010,
/// AC-25, AC-27).
#[derive(Debug, Clone, Default)]
pub struct AnimatedValues {
    /// Every value, in the order to apply them.
    pub values: Vec<AnimatedValue>,
}

/// A transition or animation event, dispatched to the element after the
/// frame (spec 0010, AC-29). `elapsed_time` is in seconds, as the event's
/// `elapsedTime` gives it.
#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::enum_variant_names,
    reason = "the names are the DOM event types"
)]
pub enum PageEvent {
    /// `transitionrun`: a transition was created.
    TransitionRun {
        /// The element.
        node: NodeId,
        /// The property.
        property: Atom,
        /// Seconds into the transition.
        elapsed_time: f64,
    },
    /// `transitionstart`: the delay ended.
    TransitionStart {
        /// The element.
        node: NodeId,
        /// The property.
        property: Atom,
        /// Seconds into the transition.
        elapsed_time: f64,
    },
    /// `transitionend`: the transition finished.
    TransitionEnd {
        /// The element.
        node: NodeId,
        /// The property.
        property: Atom,
        /// Seconds into the transition: its duration.
        elapsed_time: f64,
    },
    /// `transitioncancel`: the transition stopped before its end.
    TransitionCancel {
        /// The element.
        node: NodeId,
        /// The property.
        property: Atom,
        /// Seconds into the transition.
        elapsed_time: f64,
    },
    /// `animationstart`: the delay ended.
    AnimationStart {
        /// The element.
        node: NodeId,
        /// The animation name.
        name: Atom,
        /// Seconds into the animation.
        elapsed_time: f64,
    },
    /// `animationiteration`: an iteration ended and another began.
    AnimationIteration {
        /// The element.
        node: NodeId,
        /// The animation name.
        name: Atom,
        /// Seconds into the animation.
        elapsed_time: f64,
    },
    /// `animationend`: the last iteration ended.
    AnimationEnd {
        /// The element.
        node: NodeId,
        /// The animation name.
        name: Atom,
        /// Seconds into the animation: its active duration.
        elapsed_time: f64,
    },
    /// `animationcancel`: the animation stopped before its end.
    AnimationCancel {
        /// The element.
        node: NodeId,
        /// The animation name.
        name: Atom,
        /// Seconds into the animation.
        elapsed_time: f64,
    },
}

impl PageEvent {
    /// The element it fires on.
    pub fn node(&self) -> NodeId {
        match self {
            Self::TransitionRun { node, .. }
            | Self::TransitionStart { node, .. }
            | Self::TransitionEnd { node, .. }
            | Self::TransitionCancel { node, .. }
            | Self::AnimationStart { node, .. }
            | Self::AnimationIteration { node, .. }
            | Self::AnimationEnd { node, .. }
            | Self::AnimationCancel { node, .. } => *node,
        }
    }

    /// The DOM event: its type, whether it is a `TransitionEvent` (else an
    /// `AnimationEvent`), the property or animation name and `elapsedTime`.
    pub fn dom(&self) -> (&'static str, bool, &Atom, f64) {
        match self {
            Self::TransitionRun {
                property,
                elapsed_time,
                ..
            } => ("transitionrun", true, property, *elapsed_time),
            Self::TransitionStart {
                property,
                elapsed_time,
                ..
            } => ("transitionstart", true, property, *elapsed_time),
            Self::TransitionEnd {
                property,
                elapsed_time,
                ..
            } => ("transitionend", true, property, *elapsed_time),
            Self::TransitionCancel {
                property,
                elapsed_time,
                ..
            } => ("transitioncancel", true, property, *elapsed_time),
            Self::AnimationStart {
                name, elapsed_time, ..
            } => ("animationstart", false, name, *elapsed_time),
            Self::AnimationIteration {
                name, elapsed_time, ..
            } => ("animationiteration", false, name, *elapsed_time),
            Self::AnimationEnd {
                name, elapsed_time, ..
            } => ("animationend", false, name, *elapsed_time),
            Self::AnimationCancel {
                name, elapsed_time, ..
            } => ("animationcancel", false, name, *elapsed_time),
        }
    }
}

/// The running transitions and animations of one page (spec 0010, AC-27).
#[derive(Debug, Clone, Default)]
pub struct AnimationSet {
    transitions: HashMap<(NodeId, PropertyId), RunningTransition>,
    animations: HashMap<(NodeId, Atom), RunningAnimation>,
    next_binding_id: u64,
    /// Queued events with the document time of the boundary each crosses.
    events: Vec<(f64, PageEvent)>,
    /// The cap's one `warn` was logged.
    warned: bool,
}

/// One transition to start: its property, the styles holding its start
/// and end values, and its timing.
#[derive(Debug, Clone)]
pub struct TransitionStart {
    /// The property.
    pub property: PropertyId,
    /// A style holding the start value.
    pub from: Rc<ComputedStyle>,
    /// A style holding the end value.
    pub to: Rc<ComputedStyle>,
    /// Seconds.
    pub duration: f64,
    /// Seconds; negative starts partway in.
    pub delay: f64,
    /// The easing.
    pub timing: TimingFunction,
}

impl AnimationSet {
    fn has_room(&mut self) -> bool {
        if self.transitions.len() + self.animations.len() < MAX_RUNNING {
            return true;
        }
        if !self.warned {
            self.warned = true;
            tracing::warn!("animation cap reached: new transitions and animations ignored");
        }
        false
    }

    fn binding(&mut self) -> BindingId {
        let id = BindingId(self.next_binding_id);
        self.next_binding_id = self.next_binding_id.saturating_add(1);
        id
    }

    /// Starts a transition on `node` at document time `now`, replacing any
    /// running for the same property, and queues `transitionrun` (spec
    /// 0010, AC-23, AC-29).
    pub fn start_transition(&mut self, node: NodeId, start: TransitionStart, now: f64) {
        if !self.transitions.contains_key(&(node, start.property)) && !self.has_room() {
            return;
        }
        let binding_id = self.binding();
        let running = RunningTransition {
            reversing_start: Rc::clone(&start.from),
            from: start.from,
            to: start.to,
            shortening: 1.0,
            start: now,
            delay: start.delay,
            duration: start.duration.max(0.0),
            timing: start.timing,
            started: false,
            binding_id,
        };
        self.queue_run(node, start.property, &running, now);
        self.transitions.insert((node, start.property), running);
    }

    fn queue_run(&mut self, node: NodeId, property: PropertyId, t: &RunningTransition, now: f64) {
        self.events.push((
            now,
            PageEvent::TransitionRun {
                node,
                property: Atom::from(property.name()),
                elapsed_time: (-t.delay).clamp(0.0, t.duration),
            },
        ));
    }

    /// The running transition of `property` on `node` meets a new end value
    /// in `after`, from the current value in `current` (CSS Transitions 1,
    /// section 3, steps 2 to 4). Returns whether it was handled: kept when
    /// already heading there, reversed with the shortened duration when
    /// `after` is its reversing start value; otherwise it is cancelled and
    /// `false` asks the caller to start a fresh one.
    pub fn retarget(
        &mut self,
        node: NodeId,
        property: PropertyId,
        current: &Rc<ComputedStyle>,
        after: &Rc<ComputedStyle>,
        spec: &TransitionSpec,
        now: f64,
    ) -> bool {
        let Some(running) = self.transitions.get(&(node, property)) else {
            return false;
        };
        if !property.differs(&running.to, after) {
            return true;
        }
        if !property.differs(&running.reversing_start, after) && spec.duration + spec.delay > 0.0 {
            let output = f64::from(running.progress(now));
            let factor = (output * running.shortening + (1.0 - running.shortening)).clamp(0.0, 1.0);
            let reversed = RunningTransition {
                from: Rc::clone(current),
                to: Rc::clone(after),
                reversing_start: Rc::clone(&running.to),
                shortening: factor,
                start: now,
                delay: if spec.delay < 0.0 {
                    spec.delay * factor
                } else {
                    spec.delay
                },
                duration: spec.duration.max(0.0) * factor,
                timing: spec.timing,
                started: false,
                binding_id: running.binding_id,
            };
            self.cancel_transition(node, property, now);
            self.queue_run(node, property, &reversed, now);
            self.transitions.insert((node, property), reversed);
            return true;
        }
        self.cancel_transition(node, property, now);
        false
    }

    /// Stops the transition of `property` on `node` before its end and
    /// queues `transitioncancel` (spec 0010, AC-29).
    pub fn cancel_transition(&mut self, node: NodeId, property: PropertyId, now: f64) {
        if let Some(t) = self.transitions.remove(&(node, property)) {
            self.events.push((
                now,
                PageEvent::TransitionCancel {
                    node,
                    property: Atom::from(property.name()),
                    elapsed_time: t.elapsed(now).clamp(0.0, t.duration),
                },
            ));
        }
    }

    /// Whether the transition of `property` on `node` already heads to the
    /// value in `style`.
    pub fn heads_to(&self, node: NodeId, property: PropertyId, style: &ComputedStyle) -> bool {
        self.transitions
            .get(&(node, property))
            .is_some_and(|t| !property.differs(&t.to, style))
    }

    /// Whether a transition of `property` runs on `node`.
    pub fn has_transition(&self, node: NodeId, property: PropertyId) -> bool {
        self.transitions.contains_key(&(node, property))
    }

    /// The properties with a running transition on `node`.
    pub fn transitioning(&self, node: NodeId) -> Vec<PropertyId> {
        self.transitions
            .keys()
            .filter(|(n, _)| *n == node)
            .map(|(_, p)| *p)
            .collect()
    }

    /// Starts the animation `spec` on `node` at `now`, playing `tracks`
    /// (from [`resolve_keyframes`]) over the element's own style `base`
    /// (spec 0010, AC-24). `order` is its index in `animation-name`.
    pub fn start_animation(
        &mut self,
        node: NodeId,
        order: usize,
        spec: &AnimationSpec,
        tracks: Option<Tracks>,
        base: Rc<ComputedStyle>,
        now: f64,
    ) {
        if !self.has_room() {
            return;
        }
        let binding_id = self.binding();
        let has_rule = tracks.is_some();
        let tracks = tracks.unwrap_or_else(|| Rc::from([]));
        self.animations.insert(
            (node, spec.name.clone()),
            RunningAnimation {
                order,
                tracks,
                has_rule,
                base,
                start: now,
                duration: spec.duration.max(0.0),
                delay: spec.delay,
                iteration_count: spec.iteration_count,
                timing: spec.timing,
                direction: spec.direction,
                fill_mode: spec.fill_mode,
                paused_at: (spec.play_state == PlayState::Paused).then_some(now),
                started: false,
                iteration: 0,
                finished: false,
                binding_id,
            },
        );
    }

    /// Whether the animation `name` exists on `node`, running, paused or
    /// held after its end.
    pub fn is_running_for(&self, node: NodeId, name: &Atom) -> bool {
        self.animations.contains_key(&(node, name.clone()))
    }

    /// Brings the animation `name` on `node` up to its current style: its
    /// place in the list and `animation-play-state`. Pausing freezes its
    /// time; resuming continues from there (spec 0010, AC-24).
    pub fn update_animation(&mut self, node: NodeId, order: usize, spec: &AnimationSpec, now: f64) {
        let Some(running) = self.animations.get_mut(&(node, spec.name.clone())) else {
            return;
        };
        running.order = order;
        match (spec.play_state, running.paused_at) {
            (PlayState::Paused, None) => running.paused_at = Some(now),
            (PlayState::Running, Some(at)) => {
                running.start += now - at;
                running.paused_at = None;
            }
            _ => {}
        }
    }

    /// The element's own style changed: missing end keyframes of its
    /// animations take the new one.
    pub fn rebase(&mut self, node: NodeId, base: &Rc<ComputedStyle>) {
        for ((n, _), running) in &mut self.animations {
            if *n == node {
                running.base = Rc::clone(base);
            }
        }
    }

    /// The elements with any entry, running or held.
    pub fn nodes(&self) -> HashSet<NodeId> {
        self.transitions
            .keys()
            .map(|(n, _)| *n)
            .chain(self.animations.keys().map(|(n, _)| *n))
            .collect()
    }

    /// The properties `node`'s animations set, which a change to its
    /// style must not transition.
    pub fn animated_properties(&self, node: NodeId) -> HashSet<PropertyId> {
        self.animations
            .iter()
            .filter(|((n, _), _)| *n == node)
            .flat_map(|(_, a)| a.tracks.iter().map(|(p, _)| *p))
            .collect()
    }

    /// Cancels every animation on `node` whose name `keep` rejects, as when
    /// its name leaves the style (spec 0010, AC-24, AC-29).
    pub fn retain_animations(&mut self, node: NodeId, now: f64, keep: impl Fn(&Atom) -> bool) {
        let gone: Vec<Atom> = self
            .animations
            .keys()
            .filter(|(n, name)| *n == node && !keep(name))
            .map(|(_, name)| name.clone())
            .collect();
        for name in gone {
            self.cancel_animation(node, name, now);
        }
    }

    fn cancel_animation(&mut self, node: NodeId, name: Atom, now: f64) {
        let Some(running) = self.animations.remove(&(node, name.clone())) else {
            return;
        };
        if running.finished || !running.has_rule || !running.started {
            return;
        }
        let elapsed = running.elapsed(now).clamp(0.0, running.active_duration());
        self.events.push((
            now,
            PageEvent::AnimationCancel {
                node,
                name,
                elapsed_time: elapsed,
            },
        ));
    }

    /// Stops everything on `node`, as when it or an ancestor is removed or
    /// set to `display: none`, and queues the cancel events (spec 0010,
    /// AC-29).
    pub fn stop_for_node(&mut self, node: NodeId, now: f64) {
        for property in self.transitioning(node) {
            self.cancel_transition(node, property, now);
        }
        let names: Vec<Atom> = self
            .animations
            .keys()
            .filter(|(n, _)| *n == node)
            .map(|(_, name)| name.clone())
            .collect();
        for name in names {
            self.cancel_animation(node, name, now);
        }
    }

    /// Whether anything still moves, so the page asks for the next frame:
    /// a transition, or an animation neither finished nor paused (spec
    /// 0010, AC-27).
    pub fn is_running(&self) -> bool {
        !self.transitions.is_empty()
            || self
                .animations
                .values()
                .any(|a| !a.finished && a.paused_at.is_none())
    }

    /// Samples everything at document time `now`: the values to write, with
    /// events queued for every boundary crossed since the last sample, at
    /// most one of each kind per entry. Finished transitions leave the set
    /// (spec 0010, AC-27, AC-29).
    pub fn sample(&mut self, now: f64) -> AnimatedValues {
        let mut animations: Vec<(NodeId, usize, AnimatedValue)> = Vec::new();
        for ((node, name), running) in &mut self.animations {
            let order = running.order;
            sample_animation(*node, name, running, now, &mut self.events, |value| {
                animations.push((*node, order, value));
            });
        }
        animations.sort_by_key(|(node, order, _)| (node.index(), *order));
        let mut values: Vec<AnimatedValue> = animations.into_iter().map(|(_, _, v)| v).collect();

        let mut finished = Vec::new();
        for (&(node, property), running) in &mut self.transitions {
            let elapsed = running.elapsed(now);
            if elapsed >= 0.0 && !running.started {
                running.started = true;
                self.events.push((
                    running.start + running.delay.max(0.0),
                    PageEvent::TransitionStart {
                        node,
                        property: Atom::from(property.name()),
                        elapsed_time: (-running.delay).clamp(0.0, running.duration),
                    },
                ));
            }
            if elapsed >= running.duration {
                self.events.push((
                    running.start + running.delay + running.duration,
                    PageEvent::TransitionEnd {
                        node,
                        property: Atom::from(property.name()),
                        elapsed_time: running.duration,
                    },
                ));
                finished.push((node, property));
                continue;
            }
            values.push(AnimatedValue {
                node,
                property,
                from: Rc::clone(&running.from),
                to: Rc::clone(&running.to),
                progress: running.progress(now),
                binding_id: running.binding_id,
            });
        }
        for key in finished {
            self.transitions.remove(&key);
        }
        AnimatedValues { values }
    }

    /// Drains the queued events in the order of the boundaries they cross,
    /// then by element (spec 0010, AC-29).
    pub fn take_events(&mut self) -> Vec<PageEvent> {
        let mut events = std::mem::take(&mut self.events);
        events.sort_by(|a, b| {
            a.0.total_cmp(&b.0)
                .then_with(|| a.1.node().index().cmp(&b.1.node().index()))
        });
        events.into_iter().map(|(_, event)| event).collect()
    }
}

/// Samples one animation at `now`, queueing the events it crosses and
/// handing each property's value to `emit`.
fn sample_animation(
    node: NodeId,
    name: &Atom,
    running: &mut RunningAnimation,
    now: f64,
    events: &mut Vec<(f64, PageEvent)>,
    mut emit: impl FnMut(AnimatedValue),
) {
    let begin = running.start + running.delay;
    let (iteration, progress, show) = match running.phase(now) {
        Phase::Before => (
            0,
            0.0,
            matches!(running.fill_mode, FillMode::Backwards | FillMode::Both),
        ),
        Phase::Active(iteration, progress) => (iteration, progress, true),
        Phase::After(iteration, progress) => (
            iteration,
            progress,
            matches!(running.fill_mode, FillMode::Forwards | FillMode::Both),
        ),
    };
    let elapsed = running.elapsed(now);
    if running.has_rule && !running.finished {
        if elapsed >= 0.0 && !running.started {
            running.started = true;
            events.push((
                begin.max(running.start),
                PageEvent::AnimationStart {
                    node,
                    name: name.clone(),
                    elapsed_time: (-running.delay).clamp(0.0, running.active_duration()),
                },
            ));
        }
        if let Phase::Active(..) = running.phase(now)
            && iteration > running.iteration
        {
            events.push((
                begin + iteration as f64 * running.duration,
                PageEvent::AnimationIteration {
                    node,
                    name: name.clone(),
                    elapsed_time: iteration as f64 * running.duration,
                },
            ));
        }
        if let Phase::After(..) = running.phase(now) {
            running.finished = true;
            let active = running.active_duration();
            events.push((
                begin + active,
                PageEvent::AnimationEnd {
                    node,
                    name: name.clone(),
                    elapsed_time: active,
                },
            ));
        }
    } else if matches!(running.phase(now), Phase::After(..)) {
        running.finished = true;
    }
    running.iteration = iteration;
    if !show {
        return;
    }
    let directed = running.directed(iteration, progress);
    for (property, stops) in running.tracks.iter() {
        if let Some((from, to, local)) = segment(stops, directed, running.timing) {
            let style = |k: &Keyframe| k.style.clone().unwrap_or_else(|| Rc::clone(&running.base));
            emit(AnimatedValue {
                node,
                property: *property,
                from: style(from),
                to: style(to),
                progress: local,
                binding_id: running.binding_id,
            });
        }
    }
}

/// The two keyframes around `progress` and the eased progress between
/// them: the first one's `animation-timing-function`, else the
/// animation's (CSS Animations 1, section 3.2).
fn segment(
    stops: &[Keyframe],
    progress: f32,
    timing: TimingFunction,
) -> Option<(&Keyframe, &Keyframe, f32)> {
    let first = stops.first()?;
    let last = stops.last()?;
    if stops.len() == 1 {
        return Some((first, last, 1.0));
    }
    let at = stops
        .windows(2)
        .position(|pair| progress < pair[1].offset)
        .unwrap_or(stops.len() - 2);
    let (a, b) = (&stops[at], &stops[at + 1]);
    let span = b.offset - a.offset;
    let local = if span <= 0.0 {
        1.0
    } else {
        ((progress - a.offset) / span).clamp(0.0, 1.0)
    };
    Some((a, b, ease(a.timing.unwrap_or(timing), local)))
}

/// One keyframe of one property, as [`resolve_keyframes`] computes it: its
/// offset (0 to 1), the style holding its value (`None` for the element's
/// own style, filling a missing end keyframe) and its timing function.
#[derive(Debug, Clone)]
pub struct Keyframe {
    /// 0 to 1.
    pub offset: f32,
    /// The style holding the value; `None` is the element's own.
    pub style: Option<Rc<ComputedStyle>>,
    /// Eases from this keyframe to the next.
    pub timing: Option<TimingFunction>,
}

/// An animation's keyframes per property, as [`resolve_keyframes`] gives
/// them.
pub type Tracks = Rc<[(PropertyId, Vec<Keyframe>)]>;

/// Computes `rule`'s keyframes for an element whose parent has `parent`'s
/// style: per property, its keyframes in offset order, with a missing
/// `0%` or `100%` filled from the element's own style. A later keyframe at
/// the same offset wins for the properties it sets (spec 0010, AC-24).
pub fn resolve_keyframes(
    rule: &KeyframesRule,
    stylist: &Stylist,
    parent: Option<&ComputedStyle>,
    root_font_size: Au,
) -> Tracks {
    let mut tracks: Vec<(PropertyId, Vec<Keyframe>)> = Vec::new();
    for keyframe in &rule.keyframes {
        let properties = css::declared_properties(&keyframe.declarations);
        if properties.is_empty() {
            continue;
        }
        let style = Rc::new(stylist.keyframe_style(&keyframe.declarations, parent, root_font_size));
        let timing = css::keyframe_timing(&keyframe.declarations);
        for &key in &keyframe.keys {
            let offset = (key / 100.0).clamp(0.0, 1.0);
            for &property in &properties {
                let at = match tracks.iter().position(|(p, _)| *p == property) {
                    Some(at) => at,
                    None => {
                        tracks.push((property, Vec::new()));
                        tracks.len() - 1
                    }
                };
                let track = &mut tracks[at].1;
                let frame = Keyframe {
                    offset,
                    style: Some(Rc::clone(&style)),
                    timing,
                };
                match track.iter_mut().find(|k| k.offset == offset) {
                    Some(existing) => *existing = frame,
                    None => track.push(frame),
                }
            }
        }
    }
    for (_, track) in &mut tracks {
        track.sort_by(|a, b| a.offset.total_cmp(&b.offset));
        if track.first().is_none_or(|k| k.offset > 0.0) {
            track.insert(
                0,
                Keyframe {
                    offset: 0.0,
                    style: None,
                    timing: None,
                },
            );
        }
        if track.last().is_none_or(|k| k.offset < 1.0) {
            track.push(Keyframe {
                offset: 1.0,
                style: None,
                timing: None,
            });
        }
    }
    Rc::from(tracks)
}

/// Eases `progress` (0 to 1) by `timing` (CSS Easing 1).
pub fn ease(timing: TimingFunction, progress: f32) -> f32 {
    let p = progress.clamp(0.0, 1.0);
    match timing {
        TimingFunction::Linear => p,
        TimingFunction::Ease => cubic_bezier(0.25, 0.1, 0.25, 1.0, p),
        TimingFunction::EaseIn => cubic_bezier(0.42, 0.0, 1.0, 1.0, p),
        TimingFunction::EaseOut => cubic_bezier(0.0, 0.0, 0.58, 1.0, p),
        TimingFunction::EaseInOut => cubic_bezier(0.42, 0.0, 0.58, 1.0, p),
        // `steps(n)` is `steps(n, jump-end)`.
        TimingFunction::Steps(n) => {
            let n = n.max(1) as f32;
            if p >= 1.0 { 1.0 } else { (p * n).floor() / n }
        }
    }
}

/// The `cubic-bezier(x1, y1, x2, y2)` curve's y at x = `t`: Newton's
/// method on x, with bisection when it stalls, to 1e-6.
fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let curve = |a: f32, b: f32, s: f32| {
        let u = 1.0 - s;
        3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
    };
    let slope = |a: f32, b: f32, s: f32| {
        let u = 1.0 - s;
        3.0 * u * u * a + 6.0 * u * s * (b - a) + 3.0 * s * s * (1.0 - b)
    };
    let mut s = t;
    for _ in 0..8 {
        let error = curve(x1, x2, s) - t;
        if error.abs() < 1e-6 {
            return curve(y1, y2, s);
        }
        let d = slope(x1, x2, s);
        if d.abs() < 1e-6 {
            break;
        }
        s -= error / d;
    }
    let (mut low, mut high) = (0.0f32, 1.0f32);
    s = t;
    for _ in 0..32 {
        let x = curve(x1, x2, s);
        if (x - t).abs() < 1e-6 {
            break;
        }
        if x < t {
            low = s;
        } else {
            high = s;
        }
        s = (low + high) / 2.0;
    }
    curve(y1, y2, s)
}

/// Each element's cascaded style under its animated one: the page writes
/// `base` plus the sampled values to the DOM as `applied`. A DOM style
/// that is no longer `applied` came from a new style pass, and is the new
/// base.
pub type Animated = HashMap<NodeId, (Rc<ComputedStyle>, Rc<ComputedStyle>)>;

/// The element's style as the cascade made it, under any animated values.
fn base_style(document: &Document, animated: &Animated, node: NodeId) -> Option<Rc<ComputedStyle>> {
    let current = document.node(node)?.as_element()?.style()?;
    Some(match animated.get(&node) {
        Some((base, applied)) if Rc::ptr_eq(current, applied) => Rc::clone(base),
        _ => Rc::clone(current),
    })
}

/// Samples `set` at `now` and writes the values over each element's own
/// style, so the next boxes, layout and paint see them; an element left
/// with no values gets its own style back. Running transform and opacity
/// values are bound (spec 0010, AC-26, AC-28).
pub fn sample_and_apply(
    document: &mut Document,
    set: &mut AnimationSet,
    animated: &mut Animated,
    now: f64,
) {
    for node in set.nodes() {
        if let Some(base) = base_style(document, animated, node)
            && animated
                .get(&node)
                .is_none_or(|(old, _)| !Rc::ptr_eq(old, &base))
        {
            set.rebase(node, &base);
        }
    }
    let sampled = set.sample(now);
    let mut by_node: HashMap<NodeId, Vec<&AnimatedValue>> = HashMap::new();
    for value in &sampled.values {
        by_node.entry(value.node).or_default().push(value);
    }
    let stale: Vec<NodeId> = animated
        .keys()
        .filter(|node| !by_node.contains_key(node))
        .copied()
        .collect();
    for node in stale {
        if let Some((base, applied)) = animated.remove(&node)
            && let Some(current) = document
                .node(node)
                .and_then(|n| n.as_element())
                .and_then(|e| e.style())
            && Rc::ptr_eq(current, &applied)
        {
            let _ = document.set_style(node, Some(base));
        }
    }
    for (node, values) in by_node {
        let Some(base) = base_style(document, animated, node) else {
            continue;
        };
        let mut style = (*base).clone();
        for value in &values {
            value.apply(&mut style);
            if value.property.is_transform() {
                Rc::make_mut(&mut style.transform).binding_id = Some(value.binding_id.0);
            }
            if value.property.is_opacity() {
                let effects = Rc::make_mut(&mut style.effects);
                if let Binding::Static(v) = effects.opacity {
                    effects.opacity = Binding::Bound {
                        id: value.binding_id,
                        value: v,
                    };
                }
            }
        }
        let applied = Rc::new(style);
        if document.set_style(node, Some(Rc::clone(&applied))).is_ok() {
            animated.insert(node, (base, applied));
        }
    }
}

/// What the motion update needs from the page's style system to start
/// animations: the stylist and the `@keyframes` lookup.
pub struct KeyframeSource<'a> {
    /// Computes keyframe declarations.
    pub stylist: &'a Stylist,
    /// The `@keyframes` rule by name, the last one winning.
    pub rule: &'a dyn Fn(&Atom) -> Option<Rc<KeyframesRule>>,
}

/// The motion update after a style pass (CSS Transitions 1 section 3, CSS
/// Animations 1 section 4): compares each element's style `before` the
/// change, animated values included, with its style now, and starts,
/// keeps, reverses or cancels its transitions; starts animations whose
/// name appeared, cancels those whose name left, and follows
/// `animation-play-state`. Elements removed, or under `display: none`,
/// lose everything (spec 0010, AC-23, AC-24, AC-29).
pub fn update(
    document: &Document,
    set: &mut AnimationSet,
    animated: &Animated,
    before: &HashMap<NodeId, Rc<ComputedStyle>>,
    keyframes: Option<&KeyframeSource<'_>>,
    now: f64,
) {
    let root_font_size = document
        .document_element()
        .and_then(|root| {
            document
                .node(root)?
                .as_element()?
                .style()
                .map(|s| s.font.size)
        })
        .unwrap_or(Au::from_px(16.0));
    let entries = set.nodes();
    let mut seen: HashSet<NodeId> = HashSet::new();
    let mut stack = vec![(document.root(), false, None::<Rc<ComputedStyle>>)];
    while let Some((id, hidden, parent)) = stack.pop() {
        let Some(node) = document.node(id) else {
            continue;
        };
        let style = node.as_element().and_then(|e| e.style()).cloned();
        let hidden = hidden
            || node.as_element().is_some()
                && style
                    .as_ref()
                    .is_none_or(|s| s.display_box.display == Display::None);
        if node.as_element().is_some() {
            seen.insert(id);
            match &style {
                Some(after) if !hidden => {
                    let base =
                        base_style(document, animated, id).unwrap_or_else(|| Rc::clone(after));
                    update_transitions(set, id, before.get(&id), after, &base, now);
                    if !after.motion.animations.is_empty() || entries.contains(&id) {
                        update_animations(
                            set,
                            id,
                            &base,
                            parent.as_deref(),
                            keyframes,
                            root_font_size,
                            now,
                        );
                    }
                }
                _ => set.stop_for_node(id, now),
            }
        }
        for child in document.children(id).collect::<Vec<_>>().into_iter().rev() {
            stack.push((child, hidden, style.clone().or_else(|| parent.clone())));
        }
    }
    for node in entries {
        if !seen.contains(&node) {
            set.stop_for_node(node, now);
        }
    }
}

/// CSS Transitions 1, section 3, for one element: `after` is its style now,
/// `base` that style without animated values.
fn update_transitions(
    set: &mut AnimationSet,
    node: NodeId,
    before: Option<&Rc<ComputedStyle>>,
    after: &Rc<ComputedStyle>,
    base: &Rc<ComputedStyle>,
    now: f64,
) {
    let Some(before) = before else {
        return;
    };
    // An element no style pass touched has nothing new to transition.
    if Rc::ptr_eq(before, after) {
        return;
    }
    let mut listed: HashMap<PropertyId, &TransitionSpec> = HashMap::new();
    for spec in base.motion.transitions.iter() {
        let properties = match &spec.property {
            Some(name) => PropertyId::expand(name),
            None => PropertyId::all().collect(),
        };
        for property in properties {
            listed.insert(property, spec);
        }
    }
    // A property no longer listed stops transitioning.
    for property in set.transitioning(node) {
        if !listed.contains_key(&property) {
            set.cancel_transition(node, property, now);
        }
    }
    if listed.is_empty() {
        return;
    }
    let animated = set.animated_properties(node);
    for (property, spec) in listed {
        if animated.contains(&property) {
            continue;
        }
        let running = set.has_transition(node, property);
        if running && set.heads_to(node, property, base) {
            continue;
        }
        if !property.differs(before, base) {
            // No change, so a transition heading elsewhere stops.
            if running {
                set.cancel_transition(node, property, now);
            }
            continue;
        }
        // Reversed, or cancelled so a fresh one starts from the current
        // value.
        if running && set.retarget(node, property, before, base, spec, now) {
            continue;
        }
        if spec.duration.max(0.0) + spec.delay <= 0.0 {
            continue;
        }
        set.start_transition(
            node,
            TransitionStart {
                property,
                from: Rc::clone(before),
                to: Rc::clone(base),
                duration: spec.duration,
                delay: spec.delay,
                timing: spec.timing,
            },
            now,
        );
    }
}

/// CSS Animations 1, section 4, for one element.
fn update_animations(
    set: &mut AnimationSet,
    node: NodeId,
    base: &Rc<ComputedStyle>,
    parent: Option<&ComputedStyle>,
    keyframes: Option<&KeyframeSource<'_>>,
    root_font_size: Au,
    now: f64,
) {
    let none = Atom::from("none");
    let specs: Vec<&AnimationSpec> = base
        .motion
        .animations
        .iter()
        .filter(|a| a.name != none)
        .collect();
    set.retain_animations(node, now, |name| specs.iter().any(|s| &s.name == name));
    let Some(source) = keyframes else {
        return;
    };
    for (order, spec) in specs.into_iter().enumerate() {
        if set.is_running_for(node, &spec.name) {
            set.update_animation(node, order, spec, now);
            continue;
        }
        let tracks = (source.rule)(&spec.name)
            .map(|rule| resolve_keyframes(&rule, source.stylist, parent, root_font_size));
        set.start_animation(node, order, spec, tracks, Rc::clone(base), now);
    }
}
