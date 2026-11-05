use std::collections::BTreeMap;

use crate::{PenPhase, PenSample};

/// No Windows calls here: stroke ownership is independent of the message transport.
#[derive(Debug, Default)]
pub(crate) struct PenState {
    pointers: BTreeMap<u32, PenSample>,
    next_stroke_id: u64,
}

impl PenState {
    pub fn contains(&self, pointer_id: u32) -> bool {
        self.pointers.contains_key(&pointer_id)
    }

    pub fn push(&mut self, mut sample: PenSample, output: &mut Vec<PenSample>) {
        let previous = self.pointers.get(&sample.pointer_id).copied();
        if let Some(previous) = previous {
            sample.time_us = sample.time_us.max(previous.time_us);
            sample.stroke_id = previous.stroke_id;
        }

        match sample.phase {
            PenPhase::Down => {
                // A second Down cannot silently overwrite an unfinished stroke.
                self.cancel_pointer(sample.pointer_id, sample.time_us, output);
                self.next_stroke_id = self
                    .next_stroke_id
                    .checked_add(1)
                    .expect("exhausted all u64 stroke identifiers");
                sample.stroke_id = Some(self.next_stroke_id);
            }
            PenPhase::Move | PenPhase::Up => {
                // Never begin a stroke from an in-contact Update or a stray Up.
                // In particular, dragging in from another window cannot start drawing.
                if sample.stroke_id.is_none() {
                    self.pointers.insert(sample.pointer_id, sample);
                    return;
                }
            }
            PenPhase::Cancel => {
                if sample.stroke_id.is_some() {
                    output.push(sample);
                }
                sample.stroke_id = None;
                self.pointers.insert(sample.pointer_id, sample);
                return;
            }
            PenPhase::Hover | PenPhase::Leave => {
                // A missing Up must not leave a contact alive indefinitely.
                self.cancel_pointer(sample.pointer_id, sample.time_us, output);
                sample.stroke_id = None;
            }
            PenPhase::Enter => {}
        }

        output.push(sample);
        if sample.phase == PenPhase::Leave {
            self.pointers.remove(&sample.pointer_id);
        } else {
            if sample.phase == PenPhase::Up {
                sample.stroke_id = None;
            }
            self.pointers.insert(sample.pointer_id, sample);
        }
    }

    pub fn cancel_pointer(&mut self, pointer_id: u32, time_us: u64, output: &mut Vec<PenSample>) {
        if let Some(last) = self.pointers.get_mut(&pointer_id)
            && last.stroke_id.is_some()
        {
            let mut canceled = *last;
            canceled.phase = PenPhase::Cancel;
            canceled.time_us = time_us.max(canceled.time_us);
            output.push(canceled);
            last.stroke_id = None;
            last.time_us = canceled.time_us;
        }
    }

    pub fn forget(&mut self, pointer_id: u32, time_us: u64, output: &mut Vec<PenSample>) {
        self.cancel_pointer(pointer_id, time_us, output);
        self.pointers.remove(&pointer_id);
    }

    pub fn cancel_all(&mut self, time_us: u64) -> Vec<PenSample> {
        let mut output = Vec::new();
        for sample in self.pointers.values_mut() {
            if sample.stroke_id.is_some() {
                sample.phase = PenPhase::Cancel;
                sample.time_us = time_us.max(sample.time_us);
                output.push(*sample);
            }
        }
        self.pointers.clear();
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PenTilt;

    fn sample(pointer_id: u32, phase: PenPhase, time_us: u64) -> PenSample {
        PenSample {
            pointer_id,
            stroke_id: None,
            time_us,
            position: [10.0, 20.0],
            pressure: Some(0.5),
            pressure_level: None,
            tilt_deg: PenTilt::default(),
            rotation_deg: None,
            eraser: false,
            inverted: false,
            barrel_button: false,
            phase,
        }
    }

    #[test]
    fn contact_keeps_one_id_outside_window_and_ends_on_up() {
        let mut state = PenState::default();
        let mut output = Vec::new();
        state.push(sample(7, PenPhase::Enter, 1), &mut output);
        state.push(sample(7, PenPhase::Hover, 2), &mut output);
        state.push(sample(7, PenPhase::Down, 3), &mut output);
        let mut outside = sample(7, PenPhase::Move, 4);
        outside.position = [-100.0, 20_000.0];
        state.push(outside, &mut output);
        state.push(sample(7, PenPhase::Up, 5), &mut output);
        state.push(sample(7, PenPhase::Hover, 6), &mut output);

        assert_eq!(output.len(), 6);
        assert_eq!(output[0].stroke_id, None);
        assert_eq!(output[1].stroke_id, None);
        let id = output[2].stroke_id.unwrap();
        assert_eq!(output[3].stroke_id, Some(id));
        assert_eq!(output[4].stroke_id, Some(id));
        assert_eq!(output[5].stroke_id, None);
        assert_eq!(output[3].position, outside.position);
    }

    #[test]
    fn dragging_in_and_stray_up_do_not_begin_strokes() {
        let mut state = PenState::default();
        let mut output = Vec::new();
        state.push(sample(7, PenPhase::Enter, 1), &mut output);
        state.push(sample(7, PenPhase::Move, 2), &mut output);
        state.push(sample(7, PenPhase::Up, 3), &mut output);
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].phase, PenPhase::Enter);
        state.push(sample(7, PenPhase::Down, 4), &mut output);
        assert!(output[1].stroke_id.is_some());
    }

    #[test]
    fn capture_loss_cancels_once_and_late_updates_cannot_restart() {
        let mut state = PenState::default();
        let mut output = Vec::new();
        state.push(sample(7, PenPhase::Down, 1), &mut output);
        let first_id = output[0].stroke_id;
        assert!(state.contains(7));
        state.forget(7, 5, &mut output);
        state.forget(7, 6, &mut output);
        assert!(!state.contains(7));
        state.push(sample(7, PenPhase::Move, 7), &mut output);
        state.push(sample(7, PenPhase::Up, 8), &mut output);
        assert_eq!(output.len(), 2);
        assert_eq!(output[1].phase, PenPhase::Cancel);
        assert_eq!(output[1].stroke_id, first_id);
        assert_eq!(output[1].time_us, 5);
        state.push(sample(7, PenPhase::Down, 9), &mut output);
        assert_ne!(output[2].stroke_id, first_id);
    }

    #[test]
    fn duplicate_down_cancels_old_contact_before_assigning_a_new_id() {
        let mut state = PenState::default();
        let mut output = Vec::new();
        state.push(sample(7, PenPhase::Down, 1), &mut output);
        state.push(sample(7, PenPhase::Down, 2), &mut output);
        assert_eq!(
            output.iter().map(|s| s.phase).collect::<Vec<_>>(),
            [PenPhase::Down, PenPhase::Cancel, PenPhase::Down]
        );
        assert_eq!(output[0].stroke_id, output[1].stroke_id);
        assert_ne!(output[0].stroke_id, output[2].stroke_id);
    }

    #[test]
    fn cancel_all_clears_multiple_contacts_but_emits_nothing_for_hover() {
        let mut state = PenState::default();
        let mut output = Vec::new();
        state.push(sample(7, PenPhase::Down, 1), &mut output);
        state.push(sample(8, PenPhase::Down, 2), &mut output);
        state.push(sample(9, PenPhase::Hover, 3), &mut output);
        let canceled = state.cancel_all(10);
        assert_eq!(canceled.len(), 2);
        assert!(
            canceled
                .iter()
                .all(|s| s.phase == PenPhase::Cancel && s.time_us == 10)
        );
        assert_ne!(canceled[0].stroke_id, canceled[1].stroke_id);
        assert!(state.cancel_all(11).is_empty());
        assert!(!state.contains(9));
    }

    #[test]
    fn pointer_id_reuse_gets_a_new_stroke_and_enter_after_down_preserves_contact() {
        let mut state = PenState::default();
        let mut output = Vec::new();
        for (phase, time) in [
            (PenPhase::Down, 1),
            (PenPhase::Enter, 1),
            (PenPhase::Move, 2),
            (PenPhase::Up, 3),
            (PenPhase::Leave, 4),
            (PenPhase::Down, 5),
        ] {
            state.push(sample(7, phase, time), &mut output);
        }
        assert_eq!(output.len(), 6);
        assert_eq!(output[0].stroke_id, output[1].stroke_id);
        assert_eq!(output[0].stroke_id, output[3].stroke_id);
        assert_eq!(output[4].stroke_id, None);
        assert_ne!(output[0].stroke_id, output[5].stroke_id);
    }

    #[test]
    fn missing_up_is_canceled_on_hover_or_leave() {
        for phase in [PenPhase::Hover, PenPhase::Leave] {
            let mut state = PenState::default();
            let mut output = Vec::new();
            state.push(sample(7, PenPhase::Down, 1), &mut output);
            state.push(sample(7, phase, 2), &mut output);
            assert_eq!(output.len(), 3);
            assert_eq!(output[1].phase, PenPhase::Cancel);
            assert_eq!(output[2].phase, phase);
            assert_eq!(output[2].stroke_id, None);
        }
    }

    #[test]
    fn native_cancel_uses_current_position_and_cannot_be_followed_by_move() {
        let mut state = PenState::default();
        let mut output = Vec::new();
        state.push(sample(7, PenPhase::Down, 10), &mut output);
        let mut canceled = sample(7, PenPhase::Cancel, 9);
        canceled.position = [200.0, 300.0];
        state.push(canceled, &mut output);
        state.push(sample(7, PenPhase::Move, 11), &mut output);
        assert_eq!(output.len(), 2);
        assert_eq!(output[1].position, canceled.position);
        assert_eq!(output[1].time_us, 10); // time never runs backwards
    }
}
