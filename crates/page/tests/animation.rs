//! Tests for the animation set: transitions are sampled frame by frame and
//! stop asking for frames once they finish (spec 0010, AC-23, AC-27).

use base::{
    atoms::{Atom, QualName},
    local_name,
};
use css::TimingFunction;
use dom::{Document, NodeId};
use page::AnimationSet;

fn scale_at(set: &mut AnimationSet, node: NodeId, frame_time: f64) -> Option<f32> {
    let values = set.sample(frame_time);
    values
        .transforms
        .get(&node)
        .copied()
        .map(|(scale, _)| scale)
}

fn div(doc: &mut Document) -> NodeId {
    doc.create_element(QualName::html(local_name!("div")))
}

#[test]
fn a_transition_scales_from_zero_to_one_over_its_duration() {
    let mut doc = Document::new("about:blank");
    let node = div(&mut doc);
    let mut set = AnimationSet::default();
    set.start_transition(
        node,
        Atom::from("transform"),
        0.0,
        1.0,
        1.0,
        0.0,
        TimingFunction::Linear,
        0.0,
    );
    assert_eq!(scale_at(&mut set, node, 0.0), Some(0.0));
    assert_eq!(scale_at(&mut set, node, 0.5), Some(0.5));
    assert_eq!(scale_at(&mut set, node, 1.0), Some(1.0));
}

#[test]
fn a_finished_transition_stops_asking_for_frames() {
    let mut doc = Document::new("about:blank");
    let node = div(&mut doc);
    let mut set = AnimationSet::default();
    set.start_transition(
        node,
        Atom::from("transform"),
        0.0,
        1.0,
        0.5,
        0.0,
        TimingFunction::Linear,
        0.0,
    );
    assert!(set.is_running());
    let _ = scale_at(&mut set, node, 0.25);
    assert!(set.is_running());
    assert_eq!(scale_at(&mut set, node, 0.5), Some(1.0));
    assert!(!set.is_running());
    // Past the end the entry is gone, so no value.
    assert_eq!(scale_at(&mut set, node, 2.0), None);
}

#[test]
fn a_delayed_transition_waits_before_moving() {
    let mut doc = Document::new("about:blank");
    let node = div(&mut doc);
    let mut set = AnimationSet::default();
    set.start_transition(
        node,
        Atom::from("transform"),
        0.0,
        1.0,
        1.0,
        0.5,
        TimingFunction::Linear,
        0.0,
    );
    assert_eq!(scale_at(&mut set, node, 0.25), Some(0.0));
    assert_eq!(scale_at(&mut set, node, 0.75), Some(0.25));
    assert_eq!(scale_at(&mut set, node, 1.25), Some(0.75));
    assert_eq!(scale_at(&mut set, node, 1.5), Some(1.0));
}

#[test]
fn easing_scales_non_linearly() {
    let mut doc = Document::new("about:blank");
    let node = div(&mut doc);
    let mut set = AnimationSet::default();
    set.start_transition(
        node,
        Atom::from("transform"),
        0.0,
        1.0,
        1.0,
        0.0,
        TimingFunction::EaseIn,
        0.0,
    );
    let halfway = scale_at(&mut set, node, 0.5).unwrap();
    // Ease-in squares progress, so at 0.5 it is 0.25.
    assert!((halfway - 0.25).abs() < 0.001);
}
