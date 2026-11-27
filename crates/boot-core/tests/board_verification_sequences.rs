//! The boot sequences `docs/board-verification.md` predicts, run through the real `decide`,
//! `trial_step` and journal on the host. The doc says what a board should print; these tests check
//! that the doc's tables follow from the code (so an error such as "reset four times" for what is
//! three resets after the upload's own reboot shows up here, not on the bench).
//!
//! The bootloader's glue is repeated here as `Board`: `m` marks Pending, a boot calls `decide`,
//! tries the candidates in order (an image can be made to fail verification), and records a trial
//! with `trial_step`; the application's `confirm` records Confirmed (only from Testing); a power
//! cycle drops everything but the flash and reads the journal again. What this does not run is the
//! bootloader's own code (`bootloader-h750/src/main.rs`) or any image verification.

use boot_abi::{ImageVersion, SLOT_STATE_CONFIRMED, SLOT_STATE_PENDING, SLOT_STATE_TESTING};
use boot_core::{decide, trial_step, BootDecision, DEFAULT_MAX_TRIALS};
use boot_journal::{JournalManager, SlotJournalState};
use boot_layout::H750_NETLITE;
use boot_nor::RamNor;

fn ver(build: u32) -> ImageVersion {
    ImageVersion {
        major: 1,
        minor: 0,
        revision: 0,
        build_num: build,
    }
}

/// What one boot did, as the log shows it.
#[derive(Debug, PartialEq, Eq)]
enum Boot {
    /// `decide: primary=P fallback=F`, the slot that ran, and `Trial recorded: N/3` if any.
    Ran {
        primary: u8,
        fallback: Option<u8>,
        slot: u8,
        trial: Option<u8>,
        primary_failed: bool,
    },
    /// No candidate: `Entering rescue`.
    Rescue,
}

struct Board {
    nor: RamNor,
    journal: JournalManager,
    /// Whether the image in each slot passes verification.
    valid: [bool; 2],
}

impl Board {
    fn new() -> Self {
        let mut nor = RamNor::new(4 * 1024 * 1024);
        let journal = JournalManager::init(&mut nor, &H750_NETLITE).unwrap();
        Board {
            nor,
            journal,
            valid: [true, true],
        }
    }

    fn state(&self, slot: u8) -> SlotJournalState {
        if slot == 0 {
            self.journal.slot_a
        } else {
            self.journal.slot_b
        }
    }

    /// The `J` line of a slot: (state, trial) as printed at the start of a boot.
    fn j(&self, slot: u8) -> (u8, u8) {
        let s = self.state(slot);
        (s.state, s.trial)
    }

    /// Rescue `m <slot>`: verify, then record Pending with the image's version. A refused image
    /// writes nothing (the answer is ERR_VERIFY).
    fn m(&mut self, slot: u8, build: u32) -> Result<(), &'static str> {
        if !self.valid[slot as usize] {
            return Err("ERR_VERIFY");
        }
        self.journal
            .append_record(
                &mut self.nor,
                slot,
                SLOT_STATE_PENDING,
                0,
                0,
                ver(build),
                0,
                0,
            )
            .unwrap();
        Ok(())
    }

    /// One boot of the bootloader.
    fn boot(&mut self) -> Boot {
        let (primary, fallback) = match decide(
            &self.journal.slot_a,
            &self.journal.slot_b,
            DEFAULT_MAX_TRIALS,
        ) {
            BootDecision::TryCandidates { primary, fallback } => (primary, fallback),
            BootDecision::EnterRescue => return Boot::Rescue,
        };
        let mut slot = primary;
        let mut primary_failed = false;
        if !self.valid[primary as usize] {
            primary_failed = true;
            match fallback {
                Some(f) if self.valid[f as usize] => slot = f,
                _ => return Boot::Rescue,
            }
        }
        let st = self.state(slot);
        let trial = trial_step(&st, DEFAULT_MAX_TRIALS).map(|step| {
            self.journal
                .append_record(
                    &mut self.nor,
                    slot,
                    step.new_state,
                    step.new_trial,
                    0,
                    st.img_ver,
                    0,
                    0,
                )
                .unwrap();
            step.new_trial
        });
        Boot::Ran {
            primary,
            fallback,
            slot,
            trial,
            primary_failed,
        }
    }

    /// The application's `confirm()`: only a slot on trial (Testing) can be confirmed.
    fn confirm(&mut self, slot: u8) -> Result<(), &'static str> {
        let t = self.state(slot);
        if t.state != SLOT_STATE_TESTING {
            return Err("ERR_INVALID_STATE");
        }
        self.journal
            .append_record(
                &mut self.nor,
                slot,
                SLOT_STATE_CONFIRMED,
                0,
                0,
                t.img_ver,
                t.min_version,
                t.key_revoke_mask,
            )
            .unwrap();
        Ok(())
    }

    /// Power removed and restored: only the flash survives.
    fn power_cycle(&mut self) {
        self.journal = JournalManager::init(&mut self.nor, &H750_NETLITE).unwrap();
    }

    /// Slot 0 holds a confirmed build 1 (the setup of cases 1 and 2).
    fn with_confirmed_slot_0() -> Self {
        let mut b = Board::new();
        b.m(0, 1).unwrap();
        assert!(matches!(
            b.boot(),
            Boot::Ran {
                slot: 0,
                trial: Some(1),
                ..
            }
        ));
        b.confirm(0).unwrap();
        assert_eq!(b.j(0).0, SLOT_STATE_CONFIRMED);
        b
    }
}

fn ran(primary: u8, fallback: Option<u8>, slot: u8, trial: Option<u8>) -> Boot {
    Boot::Ran {
        primary,
        fallback,
        slot,
        trial,
        primary_failed: false,
    }
}

const PENDING: u8 = SLOT_STATE_PENDING;
const TESTING: u8 = SLOT_STATE_TESTING;
const CONFIRMED: u8 = SLOT_STATE_CONFIRMED;

#[test]
fn the_codes_the_doc_prints_are_the_ones_in_the_journal() {
    // "02 Pending, 03 Testing, 04 Confirmed" and "3 trials" in the doc
    assert_eq!((PENDING, TESTING, CONFIRMED), (2, 3, 4));
    assert_eq!(DEFAULT_MAX_TRIALS, 3);
}

/// Case 1 (and case 2: the watchdog only makes the resets happen by themselves).
#[test]
fn case_1_testing_trials_then_rollback() {
    let mut b = Board::with_confirmed_slot_0();
    // upload noconfirm.bin (build 2) to slot 1: `m 1`, then the tool reboots the board
    b.m(1, 2).unwrap();
    assert_eq!(b.j(1), (PENDING, 0)); // `J B state=02 trial=0 ver=1.0.0+2`
    assert_eq!(b.state(1).img_ver, ver(2));

    // boot 1 is the upload's own reboot; boots 2, 3 and 4 are the three resets by hand
    let expected = [
        ((PENDING, 0), ran(1, Some(0), 1, Some(1))),
        ((TESTING, 1), ran(1, Some(0), 1, Some(2))),
        ((TESTING, 2), ran(1, Some(0), 1, Some(3))),
        ((TESTING, 3), ran(0, None, 0, None)), // out of trials: slot 0, `fallback=-`, no trial line
    ];
    for (n, (j_b, boot)) in expected.into_iter().enumerate() {
        assert_eq!(b.j(1), j_b, "boot {}: the `J B` line", n + 1);
        assert_eq!(b.boot(), boot, "boot {}", n + 1);
        // the no-confirm app never confirms; on trial it would have said so
    }
    // one reset more changes nothing: still slot 0, still no trial
    assert_eq!(b.boot(), ran(0, None, 0, None));
    assert_eq!(
        b.j(1),
        (TESTING, 3),
        "slot 1 stays used up until a new image is marked"
    );
}

/// Case 3, with a refused `m`.
#[test]
fn case_3_a_corrupt_image_writes_nothing_and_a_confirmed_slot_is_then_tried_and_fails() {
    let mut b = Board::with_confirmed_slot_0();
    let before = (
        b.journal.next_seq,
        b.journal.next_record_offset,
        b.journal.generation,
    );
    b.valid[0] = false; // corrupt.bin is in slot 0 now
    assert_eq!(b.m(0, 1), Err("ERR_VERIFY"));
    assert_eq!(
        (
            b.journal.next_seq,
            b.journal.next_record_offset,
            b.journal.generation
        ),
        before,
        "no journal record is written"
    );
    assert_eq!(
        b.j(0),
        (CONFIRMED, 0),
        "the journal still shows slot 0 as it was"
    );
    // reset: slot 0 is Confirmed, so it is tried; with no other slot: rescue
    assert_eq!(b.boot(), Boot::Rescue);
}

#[test]
fn case_3_with_a_good_image_in_the_other_slot_the_fallback_runs() {
    let mut b = Board::with_confirmed_slot_0();
    b.m(1, 2).unwrap();
    assert!(matches!(b.boot(), Boot::Ran { slot: 1, .. }));
    b.confirm(1).unwrap(); // both Confirmed; build 2 in slot 1 is newer
    b.valid[1] = false; // the newer one is corrupt
    assert_eq!(
        b.boot(),
        Boot::Ran {
            primary: 1,
            fallback: Some(0),
            slot: 0,
            trial: None,
            primary_failed: true
        },
        "`Primary candidate failed. Trying fallback slot.`"
    );
}

/// Case 7, as the doc says to do it: `--no-reboot` on the first upload, so both are Pending.
#[test]
fn case_7_version_selection_with_the_first_upload_not_rebooting() {
    let mut b = Board::new();
    b.m(0, 2).unwrap(); // ver-2.bin to slot 0, --no-reboot
    b.m(1, 3).unwrap(); // ver-3.bin to slot 1, the tool reboots
    assert_eq!((b.j(0), b.j(1)), ((PENDING, 0), (PENDING, 0)));

    // boot 1: both Pending, the newer version first; the app confirms
    assert_eq!(b.boot(), ran(1, Some(0), 1, Some(1)));
    b.confirm(1).unwrap();
    // boot 2: A Pending (02), B Confirmed (04): Pending beats Confirmed, build 2 runs
    assert_eq!((b.j(0), b.j(1)), ((PENDING, 0), (CONFIRMED, 0)));
    assert_eq!(b.boot(), ran(0, Some(1), 0, Some(1)));
    b.confirm(0).unwrap();
    // boot 3: both Confirmed: the newer version again
    assert_eq!((b.j(0).0, b.j(1).0), (CONFIRMED, CONFIRMED));
    assert_eq!(b.boot(), ran(1, Some(0), 1, None));
}

/// What happens if the first upload does reboot (the doc says why `--no-reboot` is needed).
#[test]
fn case_7_without_no_reboot_the_slots_are_not_both_pending() {
    let mut b = Board::new();
    b.m(0, 2).unwrap(); // upload 1, the tool reboots: build 2 runs and confirms
    assert_eq!(b.boot(), ran(0, None, 0, Some(1)));
    b.confirm(0).unwrap();
    b.m(1, 3).unwrap(); // upload 2
                        // slot 1 is Pending, slot 0 Confirmed: build 3 runs, then both are Confirmed: build 3 again.
                        // The order is 2, 3, 3: not the 3, 2, 3 the case is meant to show.
    assert_eq!(b.boot(), ran(1, Some(0), 1, Some(1)));
    b.confirm(1).unwrap();
    assert_eq!(b.boot(), ran(1, Some(0), 1, None));
}

/// "The version only breaks a tie between slots in the same state (then slot A)", in the doc.
#[test]
fn the_tie_break_between_slots_in_the_same_state_is_the_newer_version_then_slot_a() {
    // (build in A, build in B, the slot that runs)
    for (a, b, runs) in [
        (3u32, 2u32, 0u8),
        (2, 3, 1),
        (2, 2, 0),
        (1, 5, 1),
        (5, 1, 0),
    ] {
        let mut board = Board::new();
        board.m(0, a).unwrap();
        board.m(1, b).unwrap();
        for slot in [0u8, 1] {
            // run each once and confirm, so both are Confirmed (the order is not under test here)
            let st = board.state(slot);
            board
                .journal
                .append_record(&mut board.nor, slot, CONFIRMED, 0, 0, st.img_ver, 0, 0)
                .unwrap();
        }
        assert_eq!((board.j(0).0, board.j(1).0), (CONFIRMED, CONFIRMED));
        let other = 1 - runs;
        assert_eq!(
            board.boot(),
            ran(runs, Some(other), runs, None),
            "A is build {a}, B is build {b}"
        );
        // the same when both are Pending: the state is the same, so the version decides again
        let mut board = Board::new();
        board.m(0, a).unwrap();
        board.m(1, b).unwrap();
        assert_eq!(
            board.boot(),
            ran(runs, Some(other), runs, Some(1)),
            "both Pending: A {a}, B {b}"
        );
    }
}

/// Case 8a: a Confirmed slot, power removed.
#[test]
fn case_8a_a_confirmed_boot_writes_nothing_and_survives_a_power_cycle() {
    let mut b = Board::with_confirmed_slot_0();
    let first = b.boot();
    let j_before = (
        b.journal.generation,
        b.journal.next_seq,
        b.journal.next_record_offset,
        b.j(0),
        b.j(1),
    );
    assert_eq!(first, ran(0, None, 0, None), "no `Trial recorded`");
    b.power_cycle();
    let j_after = (
        b.journal.generation,
        b.journal.next_seq,
        b.journal.next_record_offset,
        b.j(0),
        b.j(1),
    );
    assert_eq!(j_after, j_before, "the same `J` lines after the power loss");
    assert_eq!(b.boot(), first, "the same decision and the same slot");
    assert_eq!(
        (
            b.journal.generation,
            b.journal.next_seq,
            b.journal.next_record_offset
        ),
        (j_before.0, j_before.1, j_before.2),
        "and the boot wrote nothing"
    );
}

/// Case 8b: on trial, power removed instead of a reset.
#[test]
fn case_8b_the_trial_count_survives_a_power_cycle() {
    let mut b = Board::with_confirmed_slot_0();
    b.m(1, 2).unwrap();
    assert_eq!(b.boot(), ran(1, Some(0), 1, Some(1))); // after the upload: `Trial recorded: 1/3`
    b.power_cycle(); // power removed, not a reset
    assert_eq!(
        b.j(1),
        (TESTING, 1),
        "the journal at the start of the next boot: `state=03 trial=1`"
    );
    assert_eq!(
        b.boot(),
        ran(1, Some(0), 1, Some(2)),
        "`Trial recorded: 2/3`"
    );
}

#[test]
fn confirm_only_works_on_a_slot_that_is_on_trial() {
    let mut b = Board::with_confirmed_slot_0();
    assert_eq!(
        b.confirm(0),
        Err("ERR_INVALID_STATE"),
        "a Confirmed slot has nothing to confirm"
    );
    b.m(1, 2).unwrap();
    assert_eq!(
        b.confirm(1),
        Err("ERR_INVALID_STATE"),
        "Pending is not Testing yet"
    );
    b.boot();
    assert_eq!(b.confirm(1), Ok(()));
}
