//! Stable numeric ordering with JavaScript subtraction-comparator behavior.

use crate::budget::Budget;
use crate::stage::StageError;

const MIN_GALLOP: usize = 7;

/// `array.sort((a, b) => key(b) - key(a))` over `items`, as V8 orders them.
pub fn sort_descending(items: &mut [usize], key: &dyn Fn(usize) -> f64, budget: &Budget) -> Result<(), StageError> {
    let n = items.len();
    budget.work(n.saturating_mul(usize::BITS as usize))?;
    // `CallCompare(x, y) < 0` with the comparator `(a, b) => key(b) - key(a)`; `NaN` is `+0`.
    let less = |x: usize, y: usize| key(y) - key(x) < 0.0;
    timsort(items, &less);
    Ok(())
}

struct Sort<'a> {
    work: &'a mut [usize],
    less: &'a dyn Fn(usize, usize) -> bool,
    runs: Vec<(usize, usize)>,
    min_gallop: usize,
}

fn timsort(work: &mut [usize], less: &dyn Fn(usize, usize) -> bool) {
    let length = work.len();
    if length < 2 { return; }
    let mut sort = Sort { work, less, runs: Vec::new(), min_gallop: MIN_GALLOP };
    let min_run = {
        let mut n = length;
        let mut remainder = 0;
        while n >= 64 { remainder |= n & 1; n >>= 1; }
        n + remainder
    };
    let mut low = 0;
    let mut remaining = length;
    while remaining != 0 {
        let mut run = sort.count_and_make_run(low, low + remaining);
        if run < min_run {
            let forced = min_run.min(remaining);
            sort.binary_insertion_sort(low, low + run, low + forced);
            run = forced;
        }
        sort.runs.push((low, run));
        sort.merge_collapse();
        low += run;
        remaining -= run;
    }
    while sort.runs.len() > 1 {
        let mut n = sort.runs.len() - 2;
        if n > 0 && sort.runs[n - 1].1 < sort.runs[n + 1].1 { n -= 1; }
        sort.merge_at(n);
    }
}

impl Sort<'_> {
    fn lt(&self, x: usize, y: usize) -> bool { (self.less)(x, y) }

    fn count_and_make_run(&mut self, low_arg: usize, high: usize) -> usize {
        let low = low_arg + 1;
        if low == high { return 1; }
        let mut run = 2;
        let descending = self.lt(self.work[low], self.work[low - 1]);
        let mut previous = self.work[low];
        for index in low + 1..high {
            let current = self.work[index];
            let order = self.lt(current, previous);
            if descending { if !order { break; } } else if order { break; }
            previous = current;
            run += 1;
        }
        if descending { self.work[low_arg..low_arg + run].reverse(); }
        run
    }

    fn binary_insertion_sort(&mut self, low: usize, start: usize, high: usize) {
        let mut start = if low == start { start + 1 } else { start };
        while start < high {
            let pivot = self.work[start];
            let (mut left, mut right) = (low, start);
            while left < right {
                let middle = left + ((right - left) >> 1);
                if self.lt(pivot, self.work[middle]) { right = middle; } else { left = middle + 1; }
            }
            self.work.copy_within(left..start, left + 1);
            self.work[left] = pivot;
            start += 1;
        }
    }

    fn invariant(&self, n: usize) -> bool {
        if n < 2 { return true; }
        self.runs[n - 2].1 > self.runs[n - 1].1 + self.runs[n].1
    }

    fn merge_collapse(&mut self) {
        while self.runs.len() > 1 {
            let mut n = self.runs.len() - 2;
            if !self.invariant(n + 1) || !self.invariant(n) {
                if self.runs[n - 1].1 < self.runs[n + 1].1 { n -= 1; }
                self.merge_at(n);
            } else if self.runs[n].1 <= self.runs[n + 1].1 {
                self.merge_at(n);
            } else {
                break;
            }
        }
    }

    fn merge_at(&mut self, i: usize) {
        let (mut base_a, mut length_a) = self.runs[i];
        let (base_b, mut length_b) = self.runs[i + 1];
        self.runs[i].1 = length_a + length_b;
        if i + 3 == self.runs.len() { self.runs[i + 1] = self.runs[i + 2]; }
        self.runs.pop();
        let key_right = self.work[base_b];
        let k = gallop_right(self.work, self.less, key_right, base_a, length_a, 0);
        base_a += k;
        length_a -= k;
        if length_a == 0 { return; }
        let key_left = self.work[base_a + length_a - 1];
        length_b = gallop_left(self.work, self.less, key_left, base_b, length_b, length_b - 1);
        if length_b == 0 { return; }
        if length_a <= length_b { self.merge_low(base_a, length_a, base_b, length_b); }
        else { self.merge_high(base_a, length_a, base_b, length_b); }
    }

    fn merge_low(&mut self, base_a: usize, length_a: usize, base_b: usize, length_b: usize) {
        let mut length_a = length_a as isize;
        let mut length_b = length_b as isize;
        let temp: Vec<usize> = self.work[base_a..base_a + length_a as usize].to_vec();
        let mut dest = base_a;
        let mut cursor_temp = 0usize;
        let mut cursor_b = base_b;
        self.work[dest] = self.work[cursor_b];
        dest += 1;
        cursor_b += 1;
        enum End { Succeed, CopyB }
        let end = 'merge: {
            length_b -= 1;
            if length_b == 0 { break 'merge End::Succeed; }
            if length_a == 1 { break 'merge End::CopyB; }
            let mut min_gallop = self.min_gallop;
            loop {
                let mut wins_a = 0usize;
                let mut wins_b = 0usize;
                loop {
                    if self.lt(self.work[cursor_b], temp[cursor_temp]) {
                        self.work[dest] = self.work[cursor_b];
                        dest += 1;
                        cursor_b += 1;
                        wins_b += 1;
                        length_b -= 1;
                        wins_a = 0;
                        if length_b == 0 { break 'merge End::Succeed; }
                        if wins_b >= min_gallop { break; }
                    } else {
                        self.work[dest] = temp[cursor_temp];
                        dest += 1;
                        cursor_temp += 1;
                        wins_a += 1;
                        length_a -= 1;
                        wins_b = 0;
                        if length_a == 1 { break 'merge End::CopyB; }
                        if wins_a >= min_gallop { break; }
                    }
                }
                min_gallop += 1;
                let mut first = true;
                while wins_a >= MIN_GALLOP || wins_b >= MIN_GALLOP || first {
                    first = false;
                    min_gallop = min_gallop.saturating_sub(1).max(1);
                    self.min_gallop = min_gallop;
                    wins_a = gallop_right(&temp, self.less, self.work[cursor_b], cursor_temp, length_a as usize, 0);
                    if wins_a > 0 {
                        self.work[dest..dest + wins_a].copy_from_slice(&temp[cursor_temp..cursor_temp + wins_a]);
                        dest += wins_a;
                        cursor_temp += wins_a;
                        length_a -= wins_a as isize;
                        if length_a == 1 { break 'merge End::CopyB; }
                        if length_a == 0 { break 'merge End::Succeed; }
                    }
                    self.work[dest] = self.work[cursor_b];
                    dest += 1;
                    cursor_b += 1;
                    length_b -= 1;
                    if length_b == 0 { break 'merge End::Succeed; }
                    wins_b = gallop_left(self.work, self.less, temp[cursor_temp], cursor_b, length_b as usize, 0);
                    if wins_b > 0 {
                        self.work.copy_within(cursor_b..cursor_b + wins_b, dest);
                        dest += wins_b;
                        cursor_b += wins_b;
                        length_b -= wins_b as isize;
                        if length_b == 0 { break 'merge End::Succeed; }
                    }
                    self.work[dest] = temp[cursor_temp];
                    dest += 1;
                    cursor_temp += 1;
                    length_a -= 1;
                    if length_a == 1 { break 'merge End::CopyB; }
                }
                min_gallop += 1;
                self.min_gallop = min_gallop;
            }
        };
        match end {
            End::Succeed => {
                if length_a > 0 {
                    let length_a = length_a as usize;
                    self.work[dest..dest + length_a].copy_from_slice(&temp[cursor_temp..cursor_temp + length_a]);
                }
            }
            End::CopyB => {
                let length_b = length_b as usize;
                self.work.copy_within(cursor_b..cursor_b + length_b, dest);
                self.work[dest + length_b] = temp[cursor_temp];
            }
        }
    }

    fn merge_high(&mut self, base_a: usize, length_a: usize, base_b: usize, length_b: usize) {
        let mut length_a = length_a as isize;
        let mut length_b = length_b as isize;
        let temp: Vec<usize> = self.work[base_b..base_b + length_b as usize].to_vec();
        let mut dest = (base_b + length_b as usize - 1) as isize;
        let mut cursor_temp = length_b - 1;
        let mut cursor_a = (base_a + length_a as usize - 1) as isize;
        self.work[dest as usize] = self.work[cursor_a as usize];
        dest -= 1;
        cursor_a -= 1;
        enum End { Succeed, CopyA }
        let end = 'merge: {
            length_a -= 1;
            if length_a == 0 { break 'merge End::Succeed; }
            if length_b == 1 { break 'merge End::CopyA; }
            let mut min_gallop = self.min_gallop;
            loop {
                let mut wins_a = 0usize;
                let mut wins_b = 0usize;
                loop {
                    if self.lt(temp[cursor_temp as usize], self.work[cursor_a as usize]) {
                        self.work[dest as usize] = self.work[cursor_a as usize];
                        dest -= 1;
                        cursor_a -= 1;
                        wins_a += 1;
                        length_a -= 1;
                        wins_b = 0;
                        if length_a == 0 { break 'merge End::Succeed; }
                        if wins_a >= min_gallop { break; }
                    } else {
                        self.work[dest as usize] = temp[cursor_temp as usize];
                        dest -= 1;
                        cursor_temp -= 1;
                        wins_b += 1;
                        length_b -= 1;
                        wins_a = 0;
                        if length_b == 1 { break 'merge End::CopyA; }
                        if wins_b >= min_gallop { break; }
                    }
                }
                min_gallop += 1;
                let mut first = true;
                while wins_a >= MIN_GALLOP || wins_b >= MIN_GALLOP || first {
                    first = false;
                    min_gallop = min_gallop.saturating_sub(1).max(1);
                    self.min_gallop = min_gallop;
                    let k = gallop_right(self.work, self.less, temp[cursor_temp as usize], base_a, length_a as usize, length_a as usize - 1);
                    wins_a = length_a as usize - k;
                    if wins_a > 0 {
                        dest -= wins_a as isize;
                        cursor_a -= wins_a as isize;
                        let from = (cursor_a + 1) as usize;
                        self.work.copy_within(from..from + wins_a, (dest + 1) as usize);
                        length_a -= wins_a as isize;
                        if length_a == 0 { break 'merge End::Succeed; }
                    }
                    self.work[dest as usize] = temp[cursor_temp as usize];
                    dest -= 1;
                    cursor_temp -= 1;
                    length_b -= 1;
                    if length_b == 1 { break 'merge End::CopyA; }
                    let k = gallop_left(&temp, self.less, self.work[cursor_a as usize], 0, length_b as usize, length_b as usize - 1);
                    wins_b = length_b as usize - k;
                    if wins_b > 0 {
                        dest -= wins_b as isize;
                        cursor_temp -= wins_b as isize;
                        let from = (cursor_temp + 1) as usize;
                        let to = (dest + 1) as usize;
                        self.work[to..to + wins_b].copy_from_slice(&temp[from..from + wins_b]);
                        length_b -= wins_b as isize;
                        if length_b == 1 { break 'merge End::CopyA; }
                        if length_b == 0 { break 'merge End::Succeed; }
                    }
                    self.work[dest as usize] = self.work[cursor_a as usize];
                    dest -= 1;
                    cursor_a -= 1;
                    length_a -= 1;
                    if length_a == 0 { break 'merge End::Succeed; }
                }
                min_gallop += 1;
                self.min_gallop = min_gallop;
            }
        };
        match end {
            End::Succeed => {
                if length_b > 0 {
                    let length_b = length_b as usize;
                    let to = (dest - (length_b as isize - 1)) as usize;
                    self.work[to..to + length_b].copy_from_slice(&temp[..length_b]);
                }
            }
            End::CopyA => {
                dest -= length_a;
                cursor_a -= length_a;
                let from = (cursor_a + 1) as usize;
                self.work.copy_within(from..from + length_a as usize, (dest + 1) as usize);
                self.work[dest as usize] = temp[cursor_temp as usize];
            }
        }
    }
}

/// V8 `GallopLeft`: the position of `key` in the sorted `array[base..base + length]`, left of equal elements.
fn gallop_left(array: &[usize], less: &dyn Fn(usize, usize) -> bool, key: usize, base: usize, length: usize, hint: usize) -> usize {
    let (length, hint) = (length as isize, hint as isize);
    let at = |offset: isize| array[(base as isize + offset) as usize];
    let mut last = 0isize;
    let mut offset = 1isize;
    if less(at(hint), key) {
        let max = length - hint;
        while offset < max {
            if !less(at(hint + offset), key) { break; }
            last = offset;
            offset = (offset << 1) + 1;
            if offset <= 0 { offset = max; }
        }
        if offset > max { offset = max; }
        last += hint;
        offset += hint;
    } else {
        let max = hint + 1;
        while offset < max {
            if less(at(hint - offset), key) { break; }
            last = offset;
            offset = (offset << 1) + 1;
            if offset <= 0 { offset = max; }
        }
        if offset > max { offset = max; }
        let previous = last;
        last = hint - offset;
        offset = hint - previous;
    }
    last += 1;
    while last < offset {
        let middle = last + ((offset - last) >> 1);
        if less(at(middle), key) { last = middle + 1; } else { offset = middle; }
    }
    offset as usize
}

/// V8 `GallopRight`: the position of `key` in the sorted `array[base..base + length]`, right of equal elements.
fn gallop_right(array: &[usize], less: &dyn Fn(usize, usize) -> bool, key: usize, base: usize, length: usize, hint: usize) -> usize {
    let (length, hint) = (length as isize, hint as isize);
    let at = |offset: isize| array[(base as isize + offset) as usize];
    let mut last = 0isize;
    let mut offset = 1isize;
    if less(key, at(hint)) {
        let max = hint + 1;
        while offset < max {
            if !less(key, at(hint - offset)) { break; }
            last = offset;
            offset = (offset << 1) + 1;
            if offset <= 0 { offset = max; }
        }
        if offset > max { offset = max; }
        let previous = last;
        last = hint - offset;
        offset = hint - previous;
    } else {
        let max = length - hint;
        while offset < max {
            if less(key, at(hint + offset)) { break; }
            last = offset;
            offset = (offset << 1) + 1;
            if offset <= 0 { offset = max; }
        }
        if offset > max { offset = max; }
        last += hint;
        offset += hint;
    }
    last += 1;
    while last < offset {
        let middle = last + ((offset - last) >> 1);
        if less(key, at(middle)) { offset = middle; } else { last = middle + 1; }
    }
    offset as usize
}

