//! Classical backtracking execution engine

use crate::api::Match;
use crate::bytesearch;
use crate::cursor;
use crate::cursor::{Backward, Direction, Forward};
use crate::exec;
use crate::indexing::{AsciiInput, ElementType, InputIndexer, Utf8Input};
#[cfg(not(feature = "utf16"))]
use crate::insn::StartPredicate;
use crate::insn::{CompiledRegex, Insn, LoopFields};
use crate::control::{ExecutionPolicy, Unbounded};
use crate::matchers::CharProperties;
use crate::position::PositionType;
use crate::scm;
use crate::scm::SingleCharMatcher;
use crate::types::{CaptureGroupID, GroupData, IP, LoopData, LoopID, MAX_CAPTURE_GROUPS};
use crate::util::DebugCheckIndex;
#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use core::ops::Range;

#[derive(Clone, Debug)]
enum BacktrackInsn<Input: InputIndexer> {
    /// Nothing more to backtrack.
    /// This "backstops" our stack.
    Exhausted,

    /// Restore the IP and position.
    SetPosition { ip: IP, pos: Input::Position },

    SetLoopData {
        id: LoopID,
        data: LoopData<Input::Position>,
    },

    SetCaptureGroup {
        id: CaptureGroupID,
        data: GroupData<Input::Position>,
    },

    EnterNonGreedyLoop {
        // The IP of the loop.
        // This is guaranteed to point to an EnterLoopInsn.
        ip: IP,
        // The input position of the loop before entering it.
        // This is used to set up backtracking that restores this position.
        orig_pos: Input::Position,
        data: LoopData<Input::Position>,
    },

    GreedyLoop1Char {
        continuation: IP,
        min: Input::Position,
        max: Input::Position,
    },

    NonGreedyLoop1Char {
        continuation: IP,
        min: Input::Position,
        max: Input::Position,
    },
}

#[derive(Debug, Default)]
struct State<Position: PositionType> {
    loops: Vec<LoopData<Position>>,
    groups: Vec<GroupData<Position>>,
}

#[derive(Debug)]
pub(crate) struct MatchAttempter<'a, Input: InputIndexer, Policy = Unbounded> {
    policy: Policy,
    re: &'a CompiledRegex,
    bts: Vec<BacktrackInsn<Input>>,
    s: State<Input::Position>,
}

impl<'a, Input: InputIndexer> MatchAttempter<'a, Input> {
    pub(crate) fn new(re: &'a CompiledRegex, entry: Input::Position) -> Self {
        Self {
            policy: Unbounded,
            re,
            bts: vec![BacktrackInsn::Exhausted],
            s: State {
                loops: vec![LoopData::new(entry); re.loops as usize],
                groups: vec![GroupData::new(); re.groups as usize],
            },
        }
    }

}

impl<'a, Input: InputIndexer, Policy: ExecutionPolicy<Input>> MatchAttempter<'a, Input, Policy> {
    #[cfg(all(feature = "std", feature = "utf16"))]
    fn try_new(re: &'a CompiledRegex, entry: Input::Position, mut policy: Policy) -> Result<Self, Policy::Error> {
        let mut bts = Vec::new();
        Self::push_to(&mut policy, &mut bts, BacktrackInsn::Exhausted)?;
        let mut loops = Vec::new();
        policy.reserve(&mut loops, re.loops as usize)?;
        for _ in 0..re.loops { policy.charge(1)?; loops.push(LoopData::new(entry)); }
        let mut groups = Vec::new();
        policy.reserve(&mut groups, re.groups as usize)?;
        for _ in 0..re.groups { policy.charge(1)?; groups.push(GroupData::new()); }
        Ok(Self { re, bts, s: State { loops, groups }, policy })
    }

    #[inline(always)]
    fn push_to(policy: &mut Policy, bts: &mut Vec<BacktrackInsn<Input>>, bt: BacktrackInsn<Input>) -> Result<(), Policy::Error> {
        policy.charge(1)?;
        policy.reserve(bts, 1)?;
        bts.push(bt);
        Ok(())
    }

    #[inline(always)]
    fn push_backtrack(&mut self, bt: BacktrackInsn<Input>) -> Result<(), Policy::Error> {
        Self::push_to(&mut self.policy, &mut self.bts, bt)
    }

    #[inline(always)]
    fn pop_backtrack(&mut self) {
        // Note we never pop the last instruction so this will never be empty.
        debug_assert!(!self.bts.is_empty());
        if cfg!(feature = "prohibit-unsafe") {
            self.bts.pop();
        } else {
            unsafe { self.bts.set_len(self.bts.len() - 1) }
        }
    }

    fn prepare_to_enter_loop(
        policy: &mut Policy,
        bts: &mut Vec<BacktrackInsn<Input>>,
        pos: Input::Position,
        loop_fields: &LoopFields,
        loop_data: &mut LoopData<Input::Position>,
    ) -> Result<(), Policy::Error> {
        Self::push_to(policy, bts, BacktrackInsn::SetLoopData {
            id: loop_fields.loop_id,
            data: *loop_data,
        })?;
        loop_data.iters += 1;
        loop_data.entry = pos;
        Ok(())
    }

    fn run_loop(
        &mut self,
        loop_fields: &'a LoopFields,
        pos: Input::Position,
        ip: IP,
    ) -> Result<Option<IP>, Policy::Error> {
        let loop_data = &mut self.s.loops[loop_fields.loop_id as usize];
        let iteration = loop_data.iters;

        let do_taken = iteration < loop_fields.max_iters;
        let do_not_taken = iteration >= loop_fields.min_iters;

        let loop_taken_ip = ip + 1;
        let loop_not_taken_ip = loop_fields.exit as IP;

        // If we have looped more than the minimum number of iterations, reject empty
        // matches. ES6 21.2.2.5.1 Note 4: "once the minimum number of
        // repetitions has been satisfied, any more expansions of Atom that match the
        // empty character sequence are not considered for further repetitions."
        if loop_data.entry == pos && iteration > loop_fields.min_iters {
            return Ok(None);
        }

        Ok(match (do_taken, do_not_taken) {
            (false, false) => {
                // No arms viable.
                None
            }
            (false, true) => {
                // Only skipping is viable.
                Some(loop_not_taken_ip)
            }
            (true, false) => {
                // Only entering is viable.
                Self::prepare_to_enter_loop(&mut self.policy, &mut self.bts, pos, loop_fields, loop_data)?;
                Some(loop_taken_ip)
            }
            (true, true) if !loop_fields.greedy => {
                // Both arms are viable; backtrack into the loop.
                let orig_pos = loop_data.entry;
                loop_data.entry = pos;
                Self::push_to(&mut self.policy, &mut self.bts, BacktrackInsn::EnterNonGreedyLoop {
                    ip,
                    orig_pos,
                    data: *loop_data,
                })?;
                Some(loop_not_taken_ip)
            }
            (true, true) => {
                debug_assert!(loop_fields.greedy, "Should be greedy");
                // Both arms are viable; backtrack out of the loop.
                Self::push_to(&mut self.policy, &mut self.bts, BacktrackInsn::SetPosition {
                    ip: loop_not_taken_ip,
                    pos,
                })?;
                Self::prepare_to_enter_loop(&mut self.policy, &mut self.bts, pos, loop_fields, loop_data)?;
                Some(loop_taken_ip)
            }
        })
    }

    // Drive the loop up to \p max times.
    // \return the position (min, max), or None on failure.
    #[inline(always)]
    fn run_scm_loop_impl<Dir: Direction, Scm: SingleCharMatcher<Input, Dir>>(
        policy: &mut Policy,
        input: &Input,
        mut pos: Input::Position,
        min: usize,
        max: usize,
        dir: Dir,
        matcher: Scm,
    ) -> Result<Option<(Input::Position, Input::Position)>, Policy::Error> {
        debug_assert!(min <= max, "min should be <= max");
        // Drive the iteration min times.
        // That tells us the min position.
        for _ in 0..min {
            policy.charge(1)?;
            if !matcher.matches(input, dir, &mut pos) {
                return Ok(None);
            }
        }
        let min_pos = pos;

        // Drive it up to the max.
        for _ in 0..(max - min) {
            policy.charge(1)?;
            let saved = pos;
            if !matcher.matches(input, dir, &mut pos) {
                pos = saved;
                break;
            }
        }
        let max_pos = pos;
        Ok(Some((min_pos, max_pos)))
    }

    // Compute the maximum position from a starting position, up to a limit.
    // This is used for lazy computation in non-greedy loops.
    fn compute_max_pos<Dir: Direction, Scm: SingleCharMatcher<Input, Dir>>(
        policy: &mut Policy,
        input: &Input,
        mut pos: Input::Position,
        limit: usize,
        dir: Dir,
        matcher: Scm,
    ) -> Result<Input::Position, Policy::Error> {
        for _ in 0..limit {
            policy.charge(1)?;
            let saved = pos;
            if !matcher.matches(input, dir, &mut pos) {
                pos = saved;
                break;
            }
        }
        Ok(pos)
    }

    // Helper function to extract the duplicated match blocks that handle different instruction types
    // with different matcher functions. This significantly reduces code duplication and compile times.
    fn with_scm_loop_impl<Dir: Direction>(
        policy: &mut Policy,
        re: &CompiledRegex,
        input: &Input,
        pos: Input::Position,
        min: usize,
        max: usize,
        dir: Dir,
        ip: IP,
    ) -> Result<Option<(Input::Position, Input::Position)>, Policy::Error> {
        match re.insns.iat(ip + 1) {
            &Insn::Char(c) => {
                let Some(c) = <<Input as InputIndexer>::Element as ElementType>::try_from(c) else { return Ok(None); };
                Self::run_scm_loop_impl(policy, input, pos, min, max, dir, scm::Char { c })
            }
            &Insn::Bracket(idx) => {
                let bc = &re.brackets[idx];
                Self::run_scm_loop_impl(policy, input, pos, min, max, dir, scm::Bracket { bc })
            }
            Insn::AsciiBracket(bitmap) => Self::run_scm_loop_impl(
                policy, input,
                pos,
                min,
                max,
                dir,
                scm::MatchByteSet { bytes: bitmap },
            ),
            Insn::MatchAny => {
                Self::run_scm_loop_impl(policy, input, pos, min, max, dir, scm::MatchAny::new())
            }
            Insn::MatchAnyExceptLineTerminator => Self::run_scm_loop_impl(
                policy, input,
                pos,
                min,
                max,
                dir,
                scm::MatchAnyExceptLineTerminator::new(),
            ),
            Insn::CharSet(chars) => {
                Self::run_scm_loop_impl(policy, input, pos, min, max, dir, scm::CharSet { chars })
            }
            &Insn::ByteSet2(bytes) => {
                Self::run_scm_loop_impl(policy, input, pos, min, max, dir, scm::MatchByteArraySet(bytes))
            }
            &Insn::ByteSet3(bytes) => {
                Self::run_scm_loop_impl(policy, input, pos, min, max, dir, scm::MatchByteArraySet(bytes))
            }
            &Insn::ByteSet4(bytes) => {
                Self::run_scm_loop_impl(policy, input, pos, min, max, dir, scm::MatchByteArraySet(bytes))
            }
            Insn::ByteSeq1(bytes) => {
                Self::run_scm_loop_impl(policy, input, pos, min, max, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq2(bytes) => {
                Self::run_scm_loop_impl(policy, input, pos, min, max, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq3(bytes) => {
                Self::run_scm_loop_impl(policy, input, pos, min, max, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq4(bytes) => {
                Self::run_scm_loop_impl(policy, input, pos, min, max, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq5(bytes) => {
                Self::run_scm_loop_impl(policy, input, pos, min, max, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq6(bytes) => {
                Self::run_scm_loop_impl(policy, input, pos, min, max, dir, scm::MatchByteSeq(bytes))
            }
            _ => {
                unreachable!("Missing SCM: {:?}", re.insns.iat(ip + 1));
            }
        }
    }

    // Helper function for compute_max_pos to avoid duplication
    fn with_scm_compute_max<Dir: Direction>(
        policy: &mut Policy,
        re: &CompiledRegex,
        input: &Input,
        pos: Input::Position,
        limit: usize,
        dir: Dir,
        ip: IP,
    ) -> Result<Option<Input::Position>, Policy::Error> {
        let result = match re.insns.iat(ip + 1) {
            &Insn::Char(c) => {
                let Some(c) = <<Input as InputIndexer>::Element as ElementType>::try_from(c) else { return Ok(None); };
                Self::compute_max_pos(policy, input, pos, limit, dir, scm::Char { c })
            }
            &Insn::Bracket(idx) => {
                let bc = &re.brackets[idx];
                Self::compute_max_pos(policy, input, pos, limit, dir, scm::Bracket { bc })
            }
            Insn::AsciiBracket(bitmap) => {
                Self::compute_max_pos(policy, input, pos, limit, dir, scm::MatchByteSet { bytes: bitmap })
            }
            Insn::MatchAny => Self::compute_max_pos(policy, input, pos, limit, dir, scm::MatchAny::new()),
            Insn::MatchAnyExceptLineTerminator => Self::compute_max_pos(
                policy, input,
                pos,
                limit,
                dir,
                scm::MatchAnyExceptLineTerminator::new(),
            ),
            Insn::CharSet(chars) => {
                Self::compute_max_pos(policy, input, pos, limit, dir, scm::CharSet { chars })
            }
            &Insn::ByteSet2(bytes) => {
                Self::compute_max_pos(policy, input, pos, limit, dir, scm::MatchByteArraySet(bytes))
            }
            &Insn::ByteSet3(bytes) => {
                Self::compute_max_pos(policy, input, pos, limit, dir, scm::MatchByteArraySet(bytes))
            }
            &Insn::ByteSet4(bytes) => {
                Self::compute_max_pos(policy, input, pos, limit, dir, scm::MatchByteArraySet(bytes))
            }
            Insn::ByteSeq1(bytes) => {
                Self::compute_max_pos(policy, input, pos, limit, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq2(bytes) => {
                Self::compute_max_pos(policy, input, pos, limit, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq3(bytes) => {
                Self::compute_max_pos(policy, input, pos, limit, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq4(bytes) => {
                Self::compute_max_pos(policy, input, pos, limit, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq5(bytes) => {
                Self::compute_max_pos(policy, input, pos, limit, dir, scm::MatchByteSeq(bytes))
            }
            Insn::ByteSeq6(bytes) => {
                Self::compute_max_pos(policy, input, pos, limit, dir, scm::MatchByteSeq(bytes))
            }
            _ => {
                unreachable!("Missing SCM: {:?}", re.insns.iat(ip + 1));
            }
        };
        Ok(Some(result?))
    }

    // Given that ip points at a loop whose body matches exactly one character, run
    // a "single character loop". The big idea here is that we don't need to save
    // our position every iteration: we know that our loop body matches a single
    // character so we can backtrack by matching a character backwards.
    // \return the next IP, or None if the loop failed.
    #[allow(clippy::too_many_arguments)]
    fn run_scm_loop<Dir: Direction>(
        &mut self,
        input: &Input,
        dir: Dir,
        pos: &mut Input::Position,
        min: usize,
        max: usize,
        ip: IP,
        greedy: bool,
    ) -> Result<Option<IP>, Policy::Error> {
        // For non-greedy loops, we can avoid computing the maximum match eagerly.
        // We'll only compute it when we need to set up backtracking.
        let (min_pos, max_pos) = if greedy {
            // For greedy loops, compute both min and max positions
            match Self::with_scm_loop_impl(&mut self.policy, self.re, input, *pos, min, max, dir, ip)? { Some(positions) => positions, None => return Ok(None) }
        } else {
            // For non-greedy loops, initially only compute the minimum position
            let Some((min_pos, _)) = Self::with_scm_loop_impl(&mut self.policy, self.re, input, *pos, min, min, dir, ip)? else { return Ok(None); };

            // For non-greedy loops, we only compute the max position if we need to set up backtracking
            let max_pos = if min < max {
                // We need to compute the max for backtracking purposes
                match Self::with_scm_compute_max(&mut self.policy, self.re, input, min_pos, max - min, dir, ip)? { Some(position) => position, None => return Ok(None) }
            } else {
                min_pos
            };
            (min_pos, max_pos)
        };

        debug_assert!(
            if Dir::FORWARD {
                min_pos <= max_pos
            } else {
                min_pos >= max_pos
            },
            "min should be <= (>=) max if cursor is tracking forwards (backwards)"
        );

        // Oh no where is the continuation? It's one past the loop body, which is one
        // past the loop. Strap in!
        let continuation = ip + 2;
        if min_pos != max_pos {
            // Backtracking is possible.
            let bti = if greedy {
                BacktrackInsn::GreedyLoop1Char {
                    continuation,
                    min: min_pos,
                    max: max_pos,
                }
            } else {
                BacktrackInsn::NonGreedyLoop1Char {
                    continuation,
                    min: min_pos,
                    max: max_pos,
                }
            };
            self.push_backtrack(bti)?;
        }

        // Start at the max (min) if greedy (nongreedy).
        *pos = if greedy { max_pos } else { min_pos };
        Ok(Some(continuation))
    }

    // Run a lookaround instruction, which is either forwards or backwards
    // (according to Direction). The half-open range
    // start_group..end_group is the range of contained capture groups.
    // \return whether we matched and negate was false, or did not match but negate
    // is true.
    fn run_lookaround<Dir: Direction>(
        &mut self, input: &Input, ip: IP, pos: Input::Position,
        start_group: CaptureGroupID, end_group: CaptureGroupID, negate: bool,
    ) -> Result<bool, Policy::Error> {
        self.policy.enter_lookaround()?;
        let outcome = self.run_lookaround_inner::<Dir>(input, ip, pos, start_group, end_group, negate);
        self.policy.leave_lookaround();
        outcome
    }

    fn run_lookaround_inner<Dir: Direction>(
        &mut self, input: &Input, ip: IP, pos: Input::Position,
        start_group: CaptureGroupID, end_group: CaptureGroupID, negate: bool,
    ) -> Result<bool, Policy::Error> {
        let range = (start_group as usize)..(end_group as usize);
        let mut saved_groups = Vec::new();
        self.policy.reserve(&mut saved_groups, range.len())?;
        for group in self.s.groups.iat(range.clone()) {
            self.policy.charge(1)?;
            saved_groups.push(*group);
        }
        let mut saved_bts = Vec::new();
        Self::push_to(&mut self.policy, &mut saved_bts, BacktrackInsn::Exhausted)?;
        core::mem::swap(&mut self.bts, &mut saved_bts);
        let outcome = self.try_at_pos(*input, ip, pos, Dir::new());
        core::mem::swap(&mut self.bts, &mut saved_bts);
        // A terminal inner error must propagate before assertion negation.
        let matched = outcome?.is_some();
        if matched && !negate {
            for (idx, cg) in saved_groups.iter().enumerate() {
                self.policy.charge(1)?;
                debug_assert!(idx + (start_group as usize) < MAX_CAPTURE_GROUPS);
                self.push_backtrack(BacktrackInsn::SetCaptureGroup {
                    id: (idx as CaptureGroupID) + start_group, data: *cg,
                })?;
            }
        } else {
            for (slot, saved) in range.zip(saved_groups) {
                self.policy.charge(1)?;
                self.s.groups[slot] = saved;
            }
        }
        Ok(matched != negate)
    }

    /// Attempt to backtrack.
    /// \return true if we backtracked, false if we exhaust the backtrack stack.
    fn try_backtrack<Dir: Direction>(
        &mut self,
        input: &Input,
        ip: &mut IP,
        pos: &mut Input::Position,
        _dir: Dir,
    ) -> Result<bool, Policy::Error> {
        loop {
            self.policy.charge(1)?;
            // We always have a single Exhausted instruction backstopping our stack,
            // so we do not need to check for empty bts.
            debug_assert!(!self.bts.is_empty(), "Backtrack stack should not be empty");
            let bt = match self.bts.last_mut() {
                Some(bt) => bt,
                None => rs_unreachable!("BT stack should never be empty"),
            };
            match bt {
                BacktrackInsn::Exhausted => return Ok(false),

                BacktrackInsn::SetPosition {
                    ip: saved_ip,
                    pos: saved_pos,
                } => {
                    *ip = *saved_ip;
                    *pos = *saved_pos;
                    self.pop_backtrack();
                    return Ok(true);
                }
                BacktrackInsn::SetLoopData { id, data } => {
                    *self.s.loops.mat(*id as usize) = *data;
                    self.pop_backtrack();
                }
                BacktrackInsn::SetCaptureGroup { id, data } => {
                    *self.s.groups.mat(*id as usize) = *data;
                    self.pop_backtrack();
                }

                &mut BacktrackInsn::EnterNonGreedyLoop {
                    ip: loop_ip,
                    orig_pos,
                    data,
                } => {
                    *ip = loop_ip + 1;
                    *pos = data.entry;
                    let loop_fields = match &self.re.insns.iat(loop_ip) {
                        Insn::EnterLoop(loop_fields) => loop_fields,
                        _ => rs_unreachable!("EnterNonGreedyLoop must point at a loop instruction"),
                    };
                    let loop_data = self.s.loops.mat(loop_fields.loop_id as usize);
                    // Need to restore the position should we backtrack out of the loop (#131).
                    *bt = BacktrackInsn::SetLoopData {
                        id: loop_fields.loop_id,
                        data: LoopData {
                            entry: orig_pos,
                            ..data
                        },
                    };
                    *loop_data = data;
                    Self::prepare_to_enter_loop(
                        &mut self.policy, &mut self.bts,
                        *pos,
                        loop_fields,
                        loop_data,
                    )?;
                    return Ok(true);
                }

                BacktrackInsn::GreedyLoop1Char {
                    continuation,
                    min,
                    max,
                } => {
                    // The match failed at the max location.
                    debug_assert!(
                        if Dir::FORWARD { max >= min } else { max <= min },
                        "max should be >= min (or <= if tracking backwards)"
                    );
                    // If min is equal to max, there is no more backtracking to be done;
                    // otherwise move opposite the direction of the cursor.
                    if *max == *min {
                        // We have backtracked this loop as far as possible.
                        self.bts.pop();
                        continue;
                    }
                    let newmax = if Dir::FORWARD {
                        input.next_left_pos(*max)
                    } else {
                        input.next_right_pos(*max)
                    };
                    if let Some(newmax) = newmax {
                        *pos = newmax;
                        *max = newmax;
                    } else {
                        rs_unreachable!("Should always be able to advance since min != max")
                    }
                    *ip = *continuation;
                    return Ok(true);
                }

                BacktrackInsn::NonGreedyLoop1Char {
                    continuation,
                    min,
                    max,
                } => {
                    // The match failed at the min location.
                    debug_assert!(
                        if Dir::FORWARD { max >= min } else { max <= min },
                        "max should be >= min (or <= if tracking backwards)"
                    );
                    if *max == *min {
                        // We have backtracked this loop as far as possible.
                        self.bts.pop();
                        continue;
                    }
                    // Move in the direction of the cursor.
                    let newmin = if Dir::FORWARD {
                        input.next_right_pos(*min)
                    } else {
                        input.next_left_pos(*min)
                    };
                    if let Some(newmin) = newmin {
                        *pos = newmin;
                        *min = newmin;
                    } else {
                        rs_unreachable!("Should always be able to advance since min != max")
                    }
                    *ip = *continuation;
                    return Ok(true);
                }
            }
        }
    }

    /// Attempt to match at a given IP and position.
    fn try_at_pos<Dir: Direction>(
        &mut self,
        inp: Input,
        mut ip: IP,
        mut pos: Input::Position,
        dir: Dir,
    ) -> Result<Option<Input::Position>, Policy::Error> {
        debug_assert!(
            self.bts.len() == 1,
            "Should be only initial exhausted backtrack insn"
        );
        // TODO: we are inconsistent about passing Input by reference or value.
        let input = &inp;
        let re = self.re;
        // These are not really loops, they are just labels that we effectively 'goto'
        // to.
        #[allow(clippy::never_loop)]
        'nextinsn: loop {
            self.policy.charge(1)?;
            'backtrack: loop {
                // Helper macro to either increment ip and go to the next insn, or backtrack.
                macro_rules! next_or_bt {
                    ($e:expr) => {
                        if $e {
                            ip += 1;
                            continue 'nextinsn;
                        } else {
                            break 'backtrack;
                        }
                    };
                }

                match re.insns.iat(ip) {
                    &Insn::Char(c) => {
                        let m = match <<Input as InputIndexer>::Element as ElementType>::try_from(c)
                        {
                            Some(c) => scm::Char { c }.matches(input, dir, &mut pos),
                            None => false,
                        };
                        next_or_bt!(m);
                    }

                    Insn::CharSet(chars) => {
                        let m = scm::CharSet { chars }.matches(input, dir, &mut pos);
                        next_or_bt!(m);
                    }

                    &Insn::ByteSet2(bytes) => {
                        next_or_bt!(scm::MatchByteArraySet(bytes).matches(input, dir, &mut pos))
                    }
                    &Insn::ByteSet3(bytes) => {
                        next_or_bt!(scm::MatchByteArraySet(bytes).matches(input, dir, &mut pos))
                    }
                    &Insn::ByteSet4(bytes) => {
                        next_or_bt!(scm::MatchByteArraySet(bytes).matches(input, dir, &mut pos))
                    }

                    Insn::ByteSeq1(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq2(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq3(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq4(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq5(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq6(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq7(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq8(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq9(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq10(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq11(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq12(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq13(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq14(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq15(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }
                    Insn::ByteSeq16(v) => {
                        next_or_bt!(cursor::try_match_lit(input, dir, &mut pos, v))
                    }

                    Insn::AsciiBracket(bitmap) => next_or_bt!(
                        scm::MatchByteSet { bytes: bitmap }.matches(input, dir, &mut pos)
                    ),

                    &Insn::Bracket(idx) => {
                        next_or_bt!(
                            scm::Bracket {
                                bc: &self.re.brackets[idx]
                            }
                            .matches(input, dir, &mut pos)
                        )
                    }

                    Insn::MatchAny => {
                        next_or_bt!(scm::MatchAny::new().matches(input, dir, &mut pos))
                    }

                    Insn::MatchAnyExceptLineTerminator => {
                        next_or_bt!(
                            scm::MatchAnyExceptLineTerminator::new().matches(input, dir, &mut pos)
                        )
                    }

                    &Insn::WordBoundary { invert } => {
                        // Copy the positions since these destructively move them.
                        let prev_wordchar = input
                            .peek_left(pos)
                            .is_some_and(Input::CharProps::is_word_char);
                        let curr_wordchar = input
                            .peek_right(pos)
                            .is_some_and(Input::CharProps::is_word_char);
                        let is_boundary = prev_wordchar != curr_wordchar;
                        next_or_bt!(is_boundary != invert)
                    }

                    &Insn::WordBoundaryUnicodeICase { invert } => {
                        let prev_wordchar = input
                            .peek_left(pos)
                            .is_some_and(Input::CharProps::is_word_char_unicode_icase);
                        let curr_wordchar = input
                            .peek_right(pos)
                            .is_some_and(Input::CharProps::is_word_char_unicode_icase);
                        let is_boundary = prev_wordchar != curr_wordchar;
                        next_or_bt!(is_boundary != invert)
                    }

                    Insn::StartOfLine { multiline } => {
                        let multiline = *multiline;
                        let matches = match input.peek_left(pos) {
                            None => true,
                            Some(c) if multiline && Input::CharProps::is_line_terminator(c) => true,
                            _ => false,
                        };
                        next_or_bt!(matches)
                    }
                    Insn::EndOfLine { multiline } => {
                        let multiline = *multiline;
                        let matches = match input.peek_right(pos) {
                            None => true, // we're at the right of the string
                            Some(c) if multiline && Input::CharProps::is_line_terminator(c) => true,
                            _ => false,
                        };
                        next_or_bt!(matches)
                    }

                    &Insn::Jump { target } => {
                        ip = target as usize;
                        continue 'nextinsn;
                    }

                    &Insn::BeginCaptureGroup(cg_idx) => {
                        let cg = self.s.groups.mat(cg_idx as usize);
                        Self::push_to(&mut self.policy, &mut self.bts, BacktrackInsn::SetCaptureGroup {
                            id: cg_idx,
                            data: *cg,
                        })?;
                        if Dir::FORWARD {
                            cg.start = Some(pos);
                            debug_assert!(
                                cg.end.is_none(),
                                "Should not have already exited capture group we are entering"
                            )
                        } else {
                            cg.end = Some(pos);
                            debug_assert!(
                                cg.start.is_none(),
                                "Should not have already exited capture group we are entering"
                            )
                        }
                        next_or_bt!(true)
                    }

                    &Insn::EndCaptureGroup(cg_idx) => {
                        let cg = self.s.groups.mat(cg_idx as usize);
                        if Dir::FORWARD {
                            debug_assert!(
                                cg.start_matched(),
                                "Capture group should have been entered"
                            );
                            cg.end = Some(pos);
                        } else {
                            debug_assert!(
                                cg.end_matched(),
                                "Capture group should have been entered"
                            );
                            cg.start = Some(pos)
                        }
                        next_or_bt!(true)
                    }

                    &Insn::ResetCaptureGroup(cg_idx) => {
                        let cg = self.s.groups.mat(cg_idx as usize);
                        Self::push_to(&mut self.policy, &mut self.bts, BacktrackInsn::SetCaptureGroup {
                            id: cg_idx,
                            data: *cg,
                        })?;
                        cg.reset();
                        next_or_bt!(true)
                    }

                    &Insn::BackRef {
                        group: cg_idx,
                        icase,
                    } => {
                        let cg = self.s.groups.mat(cg_idx as usize);
                        // Backreferences to a capture group that did not match always succeed (ES5
                        // 15.10.2.9).
                        // Note we may be in the capture group we are examining, e.g. /(abc\1)/.
                        let matched;
                        if let Some(orig_range) = cg.as_range() {
                            matched = self.policy.backref(input, dir, orig_range, &mut pos, icase)?;
                        } else {
                            // This group has not been exited and so the match succeeds (ES6
                            // 21.2.2.9).
                            matched = true;
                        }
                        next_or_bt!(matched)
                    }

                    &Insn::Lookahead {
                        negate,
                        start_group,
                        end_group,
                        continuation,
                    } => {
                        if self.run_lookaround::<Forward>(
                            input,
                            ip + 1,
                            pos,
                            start_group,
                            end_group,
                            negate,
                        )? {
                            ip = continuation as IP;
                            continue 'nextinsn;
                        } else {
                            break 'backtrack;
                        }
                    }

                    &Insn::Lookbehind {
                        negate,
                        start_group,
                        end_group,
                        continuation,
                    } => {
                        if self.run_lookaround::<Backward>(
                            input,
                            ip + 1,
                            pos,
                            start_group,
                            end_group,
                            negate,
                        )? {
                            ip = continuation as IP;
                            continue 'nextinsn;
                        } else {
                            break 'backtrack;
                        }
                    }

                    &Insn::Alt { secondary } => {
                        self.push_backtrack(BacktrackInsn::SetPosition {
                            ip: secondary as IP,
                            pos,
                        })?;
                        next_or_bt!(true);
                    }

                    Insn::EnterLoop(fields) => {
                        // Entering a loop, not re-entering it.
                        self.s.loops.mat(fields.loop_id as usize).iters = 0;
                        match self.run_loop(fields, pos, ip)? {
                            Some(next_ip) => {
                                ip = next_ip;
                                continue 'nextinsn;
                            }
                            None => {
                                break 'backtrack;
                            }
                        }
                    }

                    &Insn::LoopAgain { begin } => {
                        let act = match re.insns.iat(begin as IP) {
                            Insn::EnterLoop(fields) => self.run_loop(fields, pos, begin as IP)?,
                            _ => rs_unreachable!("EnterLoop should always refer to loop field"),
                        };
                        match act {
                            Some(next_ip) => {
                                ip = next_ip;
                                continue 'nextinsn;
                            }
                            None => break 'backtrack,
                        }
                    }

                    &Insn::Loop1CharBody {
                        min_iters,
                        max_iters,
                        greedy,
                    } => {
                        if let Some(next_ip) = self
                            .run_scm_loop(input, dir, &mut pos, min_iters, max_iters, ip, greedy)?
                        {
                            ip = next_ip;
                            continue 'nextinsn;
                        } else {
                            break 'backtrack;
                        }
                    }

                    Insn::Goal => {
                        // Keep all but the initial give-up bts.
                        self.policy.charge(self.bts.len() - 1)?;
                        self.bts.truncate(1);
                        return Ok(Some(pos));
                    }

                    Insn::JustFail => {
                        break 'backtrack;
                    }
                }
            }

            // This after the backtrack loop.
            // A break 'backtrack will jump here.
            if self.try_backtrack(input, &mut ip, &mut pos, dir)? {
                continue 'nextinsn;
            } else {
                // We have exhausted the backtracking stack.
                debug_assert!(self.bts.len() == 1, "Should have exhausted backtrack stack");
                return Ok(None);
            }
        }

        // This is outside the nextinsn loop.
        // It is an error to get here.
        // Every instruction should either continue 'nextinsn, or break 'backtrack.
        {
            #![allow(unreachable_code)]
            rs_unreachable!("Should not fall to end of nextinsn loop")
        }
    }
}

#[derive(Debug)]
pub struct BacktrackExecutor<'r, Input: InputIndexer> {
    input: Input,
    matcher: MatchAttempter<'r, Input>,
}

#[cfg(feature = "utf16")]
impl<'r, Input: InputIndexer> BacktrackExecutor<'r, Input> {
    pub(crate) fn new(input: Input, matcher: MatchAttempter<'r, Input>) -> Self {
        Self { input, matcher }
    }
}

impl<Input: InputIndexer> BacktrackExecutor<'_, Input> {
    fn successful_match(&mut self, start: Input::Position, end: Input::Position) -> Match {
        // We want to simultaneously map our groups to offsets, and clear the groups.
        // A for loop is the easiest way to do this while satisfying the borrow checker.
        // TODO: avoid allocating so much.
        let mut captures = Vec::new();
        captures.reserve_exact(self.matcher.s.groups.len());
        for gd in self.matcher.s.groups.iter_mut() {
            captures.push(match gd.as_range() {
                Some(r) => Some(Range {
                    start: self.input.pos_to_offset(r.start),
                    end: self.input.pos_to_offset(r.end),
                }),
                None => None,
            });
            gd.start = None;
            gd.end = None;
        }
        Match {
            range: self.input.pos_to_offset(start)..self.input.pos_to_offset(end),
            captures,
            group_names: self.matcher.re.group_names.clone(),
        }
    }

    /// \return the next match for an anchored regex that only matches at the start.
    /// This avoids any string searching and only tries matching at the given position.
    #[cfg(not(feature = "utf16"))]
    fn next_match_anchored(
        &mut self,
        pos: Input::Position,
        next_start: &mut Option<Input::Position>,
    ) -> Option<Match> {
        let inp = self.input;
        // For anchored regexes, only try matching at the current position
        if let Some(end) = self.matcher.try_at_pos(inp, 0, pos, Forward::new()).unwrap_or_else(|never| match never {}) {
            // If we matched the empty string, we have to increment.
            if end != pos {
                *next_start = Some(end)
            } else {
                *next_start = inp.next_right_pos(end);
            }
            Some(self.successful_match(pos, end))
        } else {
            // Anchored regex failed to match at this position, no more matches
            None
        }
    }

    /// \return the next match, searching the remaining bytes using the given
    /// prefix searcher to quickly find the first potential match location.
    fn next_match_with_prefix_search<PrefixSearch: bytesearch::ByteSearcher>(
        &mut self,
        mut pos: Input::Position,
        next_start: &mut Option<Input::Position>,
        prefix_search: &PrefixSearch,
    ) -> Option<Match> {
        let inp = self.input;
        loop {
            // Find the next start location, or None if none.
            // Don't try this unless CODE_UNITS_ARE_BYTES - i.e. don't do byte searches
            // on UTF-16 or UCS2.
            if Input::CODE_UNITS_ARE_BYTES {
                pos = inp.find_bytes(pos, prefix_search)?;
            }
            if let Some(end) = self.matcher.try_at_pos(inp, 0, pos, Forward::new()).unwrap_or_else(|never| match never {}) {
                // If we matched the empty string, we have to increment.
                if end != pos {
                    *next_start = Some(end)
                } else {
                    *next_start = inp.next_right_pos(end);
                }
                return Some(self.successful_match(pos, end));
            }
            // Didn't find it at this position, try the next one.
            pos = inp.next_right_pos(pos)?;
        }
    }
}

impl<Input: InputIndexer> exec::MatchProducer for BacktrackExecutor<'_, Input> {
    type Position = Input::Position;

    fn initial_position(&self, offset: usize) -> Option<Self::Position> {
        self.input.try_move_right(self.input.left_end(), offset)
    }

    fn next_match(
        &mut self,
        pos: Input::Position,
        next_start: &mut Option<Input::Position>,
    ) -> Option<Match> {
        // When UTF-16 support is active prefix search is not used due to the different encoding.
        #[cfg(feature = "utf16")]
        return self.next_match_with_prefix_search(pos, next_start, &bytesearch::EmptyString {});

        #[cfg(not(feature = "utf16"))]
        match &self.matcher.re.start_pred {
            StartPredicate::Arbitrary => {
                self.next_match_with_prefix_search(pos, next_start, &bytesearch::EmptyString {})
            }
            StartPredicate::StartAnchored => self.next_match_anchored(pos, next_start),
            StartPredicate::ByteSet1(bytes) => {
                self.next_match_with_prefix_search(pos, next_start, bytes)
            }
            StartPredicate::ByteSet2(bytes) => {
                self.next_match_with_prefix_search(pos, next_start, bytes)
            }
            StartPredicate::ByteSet3(bytes) => {
                self.next_match_with_prefix_search(pos, next_start, bytes)
            }
            StartPredicate::ByteSeq(bytes) => {
                self.next_match_with_prefix_search(pos, next_start, bytes.as_ref())
            }
            StartPredicate::ByteBracket(bitmap) => {
                self.next_match_with_prefix_search(pos, next_start, bitmap)
            }
        }
    }
}

impl<'r, 't> exec::Executor<'r, 't> for BacktrackExecutor<'r, Utf8Input<'t>> {
    type AsAscii = BacktrackExecutor<'r, AsciiInput<'t>>;

    fn new(re: &'r CompiledRegex, text: &'t str) -> Self {
        let input = Utf8Input::new(text, re.flags.unicode);
        Self {
            input,
            matcher: MatchAttempter::new(re, input.left_end()),
        }
    }
}

impl<'r, 't> exec::Executor<'r, 't> for BacktrackExecutor<'r, AsciiInput<'t>> {
    type AsAscii = BacktrackExecutor<'r, AsciiInput<'t>>;

    fn new(re: &'r CompiledRegex, text: &'t str) -> Self {
        let input = AsciiInput::new(text, re.flags.unicode);
        Self {
            input,
            matcher: MatchAttempter::new(re, input.left_end()),
        }
    }
}

#[cfg(all(feature = "std", feature = "utf16"))]
/// With `anchored`, only a match that starts at `start` is tried.
pub(crate) fn find_first_controlled<'r, Input: crate::indexing::ControlledInput>(
    re: &'r CompiledRegex, input: Input, start: usize, anchored: bool, budget: &mut crate::MatchBudget<'_>,
) -> Result<Option<crate::ControlledMatch<'r>>, crate::MatchAbort> {
    budget.check()?;
    let Some(mut pos) = input.try_move_right(input.left_end(), start) else {
        budget.check()?;
        return Ok(None);
    };
    let mut matcher = MatchAttempter::try_new(re, input.left_end(), crate::control::Checked(budget))?;
    loop {
        matcher.policy.0.charge(1)?;
        if let Some(end) = matcher.try_at_pos(input, 0, pos, Forward::new())? {
            let mut captures = Vec::new();
            matcher.policy.0.reserve(&mut captures, matcher.s.groups.len())?;
            for group in &matcher.s.groups {
                matcher.policy.0.charge(1)?;
                captures.push(group.as_range().map(|range|
                    input.pos_to_offset(range.start)..input.pos_to_offset(range.end)));
            }
            matcher.policy.0.check()?;
            return Ok(Some(crate::ControlledMatch {
                range: input.pos_to_offset(pos)..input.pos_to_offset(end), captures,
                group_names: &re.group_names,
            }));
        }
        let next = if anchored { None } else { input.next_right_pos(pos) };
        let Some(next) = next else { matcher.policy.0.check()?; return Ok(None); };
        pos = next;
    }
}


#[cfg(all(test, feature = "std", feature = "utf16"))]
mod controlled_assertion_tests {
    use super::*;
    use crate::{MatchAbort, MatchBudget, MatchLimits};
    use crate::indexing::Ucs2Input;

    #[test]
    fn positive_assertion_restoration_charges_each_saved_capture() {
        let pattern = format!("(?=(?:z{}|a))a", "()".repeat(32));
        let mut ir = crate::parse::try_parse(pattern.chars().map(u32::from), crate::Flags::default()).unwrap();
        crate::optimizer::optimize(&mut ir);
        let re = crate::emit::emit(&ir);
        let input = Ucs2Input::new(&[97], false);
        let mut budget = MatchBudget::new(MatchLimits { work_units: 120, allocation_bytes: 1_000_000, lookaround_depth: 8 }, None, None);
        let mut matcher = MatchAttempter::try_new(&re, input.left_end(), crate::control::Checked(&mut budget)).unwrap();
        // The same checked reservation helper accounts for the outer stack before
        // entry, isolating restoration work from allocator-dependent growth copies.
        matcher.policy.0.reserve(&mut matcher.bts, 32).unwrap();
        let (ip, insn) = re.insns.iter().enumerate().find(|(_, insn)| matches!(insn, Insn::Lookahead { .. })).unwrap();
        let Insn::Lookahead { start_group, end_group, negate, .. } = insn else { unreachable!() };
        let outcome = matcher.run_lookaround::<Forward>(&input, ip + 1, input.left_end(), *start_group, *end_group, *negate);
        assert_eq!(outcome, Err(MatchAbort::WorkLimit));
        assert_eq!(matcher.policy.0.usage().active_lookaround_depth, 0);
    }

    #[test]
    fn inner_abort_propagates_before_negative_assertion_result() {
        for pattern in ["(?!(?:a+)+$)", "(?<!(?:a+)+)"] {
            let mut ir = crate::parse::try_parse(pattern.chars().map(u32::from), crate::Flags::default()).unwrap();
            crate::optimizer::optimize(&mut ir);
            let re = crate::emit::emit(&ir);
            let text = [97u16; 64];
            let input = Ucs2Input::new(&text, false);
            let mut budget = MatchBudget::new(MatchLimits { work_units: 16, allocation_bytes: 1_000_000, lookaround_depth: 8 }, None, None);
            let mut matcher = MatchAttempter::try_new(&re, input.left_end(), crate::control::Checked(&mut budget)).unwrap();
            let (ip, insn) = re.insns.iter().enumerate().find(|(_, insn)| matches!(insn, Insn::Lookahead { .. } | Insn::Lookbehind { .. })).unwrap();
            let outcome = match insn {
                Insn::Lookahead { start_group, end_group, negate, .. } => matcher.run_lookaround::<Forward>(
                    &input, ip + 1, input.left_end(), *start_group, *end_group, *negate),
                Insn::Lookbehind { start_group, end_group, negate, .. } => matcher.run_lookaround::<Backward>(
                    &input, ip + 1, input.right_end(), *start_group, *end_group, *negate),
                _ => unreachable!(),
            };
            assert_eq!(outcome, Err(MatchAbort::WorkLimit), "{pattern}");
            assert_eq!(matcher.policy.0.usage().active_lookaround_depth, 0);
        }
    }
}
