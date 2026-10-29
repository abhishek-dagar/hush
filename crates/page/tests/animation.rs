//! Tests for the animation set: transitions and animations are sampled on
//! a fake clock, fire their events once each, and stop asking for frames
//! once they finish (spec 0010, AC-23, AC-24, AC-27, AC-29).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use std::rc::Rc;

use base::{
    atoms::{Atom, QualName},
    local_name,
};
use css::{
    AnimationDirection, AnimationSpec, ComputedStyle, FillMode, PlayState, PropertyId,
    TimingFunction, TransitionSpec,
};
use dom::{Document, NodeId};
use page::{AnimationSet, Keyframe, PageEvent, TransitionStart};

fn scale_property() -> PropertyId {
    PropertyId::expand("scale")[0]
}

/// A style whose `scale` is `s`.
fn scaled(s: f32) -> Rc<ComputedStyle> {
    let mut style = ComputedStyle::default();
    Rc::make_mut(&mut style.transform).scale = Some((s, None));
    Rc::new(style)
}

/// The `scale` `set` gives `node` at `now`, written over a plain style.
fn scale_at(set: &mut AnimationSet, node: NodeId, now: f64) -> Option<f32> {
    let values = set.sample(now);
    let mut style = ComputedStyle::default();
    let mut found = false;
    for value in values.values.iter().filter(|v| v.node == node) {
        value.apply(&mut style);
        found = true;
    }
    found.then(|| style.transform.scale.map_or(1.0, |(x, _)| x))
}

/// All binding ids sampled for `node` this frame, as a sorted set.
fn binding_ids(set: &mut AnimationSet, node: NodeId, now: f64) -> Vec<u64> {
    let mut ids: Vec<u64> = set
        .sample(now)
        .values
        .iter()
        .filter(|v| v.node == node)
        .map(|v| v.binding_id.0)
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn div(doc: &mut Document) -> NodeId {
    doc.create_element(QualName::html(local_name!("div")))
}

fn transition(
    from: f32,
    to: f32,
    duration: f64,
    delay: f64,
    timing: TimingFunction,
) -> TransitionStart {
    TransitionStart {
        property: scale_property(),
        from: scaled(from),
        to: scaled(to),
        duration,
        delay,
        timing,
    }
}

/// `pulse`: `scale` from 1 to 2, linear.
fn pulse() -> Rc<[(PropertyId, Vec<Keyframe>)]> {
    let frame = |offset: f32, s: f32| Keyframe {
        offset,
        style: Some(scaled(s)),
        timing: None,
    };
    Rc::from([(scale_property(), vec![frame(0.0, 1.0), frame(1.0, 2.0)])])
}

fn animation(name: &str, duration: f64, delay: f64, iterations: Option<f64>) -> AnimationSpec {
    AnimationSpec {
        name: Atom::from(name),
        keyframes: None,
        duration,
        delay,
        iteration_count: iterations,
        timing: TimingFunction::Linear,
        direction: AnimationDirection::Normal,
        fill_mode: FillMode::None,
        play_state: PlayState::Running,
    }
}

fn start(set: &mut AnimationSet, node: NodeId, spec: &AnimationSpec) {
    set.start_animation(node, 0, spec, Some(pulse()), scaled(1.0), 0.0);
}

fn close(actual: Option<f32>, expected: f32) {
    let actual = actual.expect("a sampled value");
    assert!((actual - expected).abs() < 0.001, "{actual} vs {expected}");
}

#[test]
fn a_transition_scales_from_zero_to_one_over_its_duration() {
    let node = div(&mut Document::new("about:blank"));
    let mut set = AnimationSet::default();
    set.start_transition(
        node,
        transition(0.0, 1.0, 1.0, 0.0, TimingFunction::Linear),
        0.0,
    );
    close(scale_at(&mut set, node, 0.0), 0.0);
    close(scale_at(&mut set, node, 0.5), 0.5);
    // Finished: it leaves the set and the element keeps its own value.
    assert_eq!(scale_at(&mut set, node, 1.0), None);
}

#[test]
fn a_finished_transition_stops_asking_for_frames() {
    let node = div(&mut Document::new("about:blank"));
    let mut set = AnimationSet::default();
    set.start_transition(
        node,
        transition(0.0, 1.0, 0.5, 0.0, TimingFunction::Linear),
        0.0,
    );
    assert!(set.is_running());
    let _ = scale_at(&mut set, node, 0.25);
    assert!(set.is_running());
    let _ = scale_at(&mut set, node, 0.5);
    assert!(!set.is_running());
    assert_eq!(scale_at(&mut set, node, 2.0), None);
}

#[test]
fn a_delayed_transition_waits_before_moving() {
    let node = div(&mut Document::new("about:blank"));
    let mut set = AnimationSet::default();
    set.start_transition(
        node,
        transition(0.0, 1.0, 1.0, 0.5, TimingFunction::Linear),
        0.0,
    );
    close(scale_at(&mut set, node, 0.25), 0.0);
    close(scale_at(&mut set, node, 0.75), 0.25);
    close(scale_at(&mut set, node, 1.25), 0.75);
}

#[test]
fn easing_follows_the_css_curves() {
    let node = div(&mut Document::new("about:blank"));
    let mut set = AnimationSet::default();
    set.start_transition(
        node,
        transition(0.0, 1.0, 1.0, 0.0, TimingFunction::EaseIn),
        0.0,
    );
    // `ease-in` is `cubic-bezier(0.42, 0, 1, 1)`.
    close(scale_at(&mut set, node, 0.5), 0.3153);
    assert!((page::ease(TimingFunction::Ease, 0.5) - 0.8024).abs() < 0.001);
    assert_eq!(page::ease(TimingFunction::Steps(4), 0.3), 0.25);
}

#[test]
fn a_keyframes_animation_runs_until_its_duration_then_stops() {
    let node = div(&mut Document::new("about:blank"));
    let mut set = AnimationSet::default();
    start(&mut set, node, &animation("pulse", 1.0, 0.0, Some(1.0)));
    assert!(set.is_running());
    close(scale_at(&mut set, node, 0.0), 1.0);
    close(scale_at(&mut set, node, 0.5), 1.5);
    // At its end, with no fill, it shows nothing and stops.
    assert_eq!(scale_at(&mut set, node, 1.0), None);
    assert!(!set.is_running());
    assert_eq!(scale_at(&mut set, node, 2.0), None);
}

#[test]
fn fill_and_direction_shape_the_timeline() {
    let node = div(&mut Document::new("about:blank"));
    let mut set = AnimationSet::default();
    let mut spec = animation("pulse", 1.0, 1.0, Some(2.5));
    spec.fill_mode = FillMode::Both;
    spec.direction = AnimationDirection::Alternate;
    start(&mut set, node, &spec);
    // Backwards fill shows the first keyframe during the delay.
    close(scale_at(&mut set, node, 0.5), 1.0);
    close(scale_at(&mut set, node, 1.25), 1.25);
    // The second iteration runs backward.
    close(scale_at(&mut set, node, 2.25), 1.75);
    // Forwards fill holds where 2.5 iterations stop: halfway through the
    // third, forward one.
    close(scale_at(&mut set, node, 9.0), 1.5);
    assert!(!set.is_running());
}

#[test]
fn a_paused_animation_holds_and_resumes_where_it_was() {
    let node = div(&mut Document::new("about:blank"));
    let mut set = AnimationSet::default();
    let mut spec = animation("pulse", 1.0, 0.0, None);
    start(&mut set, node, &spec);
    close(scale_at(&mut set, node, 0.25), 1.25);
    spec.play_state = PlayState::Paused;
    set.update_animation(node, 0, &spec, 0.25);
    assert!(!set.is_running(), "paused, so no frames");
    close(scale_at(&mut set, node, 5.0), 1.25);
    spec.play_state = PlayState::Running;
    set.update_animation(node, 0, &spec, 5.0);
    close(scale_at(&mut set, node, 5.25), 1.5);
}

#[test]
fn a_keyframes_animation_keeps_a_stable_binding_id() {
    let node = div(&mut Document::new("about:blank"));
    let mut set = AnimationSet::default();
    start(&mut set, node, &animation("pulse", 1.0, 0.0, Some(1.0)));
    assert_eq!(binding_ids(&mut set, node, 0.0), vec![0]);
    assert_eq!(binding_ids(&mut set, node, 0.5), vec![0]);
    // A different animation on the same node gets a different id.
    start(&mut set, node, &animation("pulse2", 1.0, 0.0, Some(1.0)));
    assert_eq!(binding_ids(&mut set, node, 0.6), vec![0, 1]);
}

/// `animationstart` fires once the delay has elapsed (spec 0010, AC-29).
#[test]
fn an_animation_fires_start_after_its_delay() {
    let node = div(&mut Document::new("about:blank"));
    let mut set = AnimationSet::default();
    start(&mut set, node, &animation("pulse", 1.0, 0.5, Some(1.0)));
    let _ = scale_at(&mut set, node, 0.25);
    assert!(
        set.take_events().is_empty(),
        "nothing fires during the delay"
    );
    let _ = scale_at(&mut set, node, 0.5);
    assert_eq!(
        set.take_events(),
        vec![PageEvent::AnimationStart {
            node,
            name: Atom::from("pulse"),
            elapsed_time: 0.0,
        }]
    );
}

/// `animationiteration` fires at the end of each iteration but not the last
/// (spec 0010, AC-29).
#[test]
fn an_animation_fires_iteration_per_iteration() {
    let node = div(&mut Document::new("about:blank"));
    let mut set = AnimationSet::default();
    start(&mut set, node, &animation("pulse", 0.5, 0.0, Some(3.0)));
    let name = Atom::from("pulse");
    let _ = scale_at(&mut set, node, 0.25);
    assert_eq!(
        set.take_events(),
        vec![PageEvent::AnimationStart {
            node,
            name: name.clone(),
            elapsed_time: 0.0,
        }]
    );
    let _ = scale_at(&mut set, node, 0.6);
    assert_eq!(
        set.take_events(),
        vec![PageEvent::AnimationIteration {
            node,
            name: name.clone(),
            elapsed_time: 0.5,
        }]
    );
    let _ = scale_at(&mut set, node, 1.1);
    assert_eq!(
        set.take_events(),
        vec![PageEvent::AnimationIteration {
            node,
            name: name.clone(),
            elapsed_time: 1.0,
        }]
    );
    let _ = scale_at(&mut set, node, 1.6);
    // The last iteration ends with animationend, not another iteration.
    assert_eq!(
        set.take_events(),
        vec![PageEvent::AnimationEnd {
            node,
            name,
            elapsed_time: 1.5,
        }]
    );
    assert!(!set.is_running());
}

/// A page hidden for a long time catches up with one event of each kind
/// (spec 0010, AC-30).
#[test]
fn a_long_jump_collapses_events() {
    let node = div(&mut Document::new("about:blank"));
    let mut set = AnimationSet::default();
    start(&mut set, node, &animation("pulse", 0.1, 0.0, None));
    let _ = scale_at(&mut set, node, 0.0);
    let _ = set.take_events();
    let _ = scale_at(&mut set, node, 10.05);
    let events = set.take_events();
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(matches!(events[0], PageEvent::AnimationIteration { .. }));
}

/// `animationcancel` and `transitioncancel` fire when an element's motion
/// is stopped mid run (spec 0010, AC-29).
#[test]
fn stopping_a_node_fires_the_cancel_events() {
    let node = div(&mut Document::new("about:blank"));
    let mut set = AnimationSet::default();
    start(&mut set, node, &animation("pulse", 10.0, 0.0, None));
    set.start_transition(
        node,
        transition(1.0, 2.0, 10.0, 0.0, TimingFunction::Linear),
        0.0,
    );
    let _ = scale_at(&mut set, node, 1.0);
    let _ = set.take_events();
    set.stop_for_node(node, 2.0);
    let events = set.take_events();
    assert!(events.contains(&PageEvent::AnimationCancel {
        node,
        name: Atom::from("pulse"),
        elapsed_time: 2.0,
    }));
    assert!(events.contains(&PageEvent::TransitionCancel {
        node,
        property: Atom::from("scale"),
        elapsed_time: 2.0,
    }));
    assert!(!set.is_running());
}

/// A change back to a running transition's start value reverses it with a
/// shortened duration (CSS Transitions 1, section 3, spec 0010, AC-23).
#[test]
fn a_transition_reversed_toward_its_start_shortens() {
    let node = div(&mut Document::new("about:blank"));
    let mut set = AnimationSet::default();
    set.start_transition(
        node,
        transition(1.0, 2.0, 1.0, 0.0, TimingFunction::Linear),
        0.0,
    );
    close(scale_at(&mut set, node, 0.5), 1.5);
    let spec = TransitionSpec {
        property: Some(Atom::from("scale")),
        duration: 1.0,
        delay: 0.0,
        timing: TimingFunction::Linear,
    };
    let reversed = set.retarget(
        node,
        scale_property(),
        &scaled(1.5),
        &scaled(1.0),
        &spec,
        0.5,
    );
    assert!(reversed);
    // Halfway done, so the way back takes half the duration.
    close(scale_at(&mut set, node, 0.75), 1.25);
    assert_eq!(scale_at(&mut set, node, 1.0), None);
    assert!(!set.is_running());
}

/// A change to a third value is not a reversal: the running transition is
/// cancelled, so a fresh one can start from the current value (spec 0010,
/// AC-23).
#[test]
fn a_transition_to_a_new_value_is_not_a_reversal() {
    let node = div(&mut Document::new("about:blank"));
    let mut set = AnimationSet::default();
    set.start_transition(
        node,
        transition(1.0, 2.0, 1.0, 0.0, TimingFunction::Linear),
        0.0,
    );
    let _ = scale_at(&mut set, node, 0.5);
    let _ = set.take_events();
    let spec = TransitionSpec {
        property: Some(Atom::from("scale")),
        duration: 1.0,
        delay: 0.0,
        timing: TimingFunction::Linear,
    };
    assert!(!set.retarget(
        node,
        scale_property(),
        &scaled(1.5),
        &scaled(3.0),
        &spec,
        0.5
    ));
    assert!(!set.is_running());
    assert!(matches!(
        set.take_events().as_slice(),
        [PageEvent::TransitionCancel { .. }]
    ));
    // Heading to the same end value keeps it.
    set.start_transition(
        node,
        transition(1.5, 3.0, 1.0, 0.0, TimingFunction::Linear),
        0.5,
    );
    assert!(set.retarget(
        node,
        scale_property(),
        &scaled(2.0),
        &scaled(3.0),
        &spec,
        0.7
    ));
    assert!(set.is_running());
}

/// Past the cap, new entries are ignored (spec 0010, AC-38).
#[test]
fn the_running_cap_holds() {
    let mut doc = Document::new("about:blank");
    let mut set = AnimationSet::default();
    for _ in 0..page::MAX_RUNNING + 5 {
        let node = div(&mut doc);
        set.start_transition(
            node,
            transition(1.0, 2.0, 1.0, 0.0, TimingFunction::Linear),
            0.0,
        );
    }
    assert_eq!(set.nodes().len(), page::MAX_RUNNING);
}
