use super::*;
use ca_ui::editor::Caret;
use std::fmt::Write as _;
use std::time::Instant;

const BLOCK: usize = 20;

/// Three inputs of `blocks` blocks: 18 shared lines, then two lines the
/// left input changes. `first` and `second` name the changed lines of the
/// left input.
fn inputs(blocks: usize, first: &str, second: &str) -> [String; 3] {
    let mut left = String::new();
    let mut center = String::new();
    let mut right = String::new();
    for block in 0..blocks {
        for line in 0..BLOCK - 2 {
            for text in [&mut left, &mut center, &mut right] {
                let _ = writeln!(text, "shared {block} {line}");
            }
        }
        let _ = writeln!(left, "{first} {block} a");
        let _ = writeln!(left, "{second} {block} b");
        for text in [&mut center, &mut right] {
            let _ = writeln!(text, "base {block} a");
            let _ = writeln!(text, "base {block} b");
        }
    }
    [left, center, right]
}

/// Open the merge and join the first changed line of every `every`th block
/// to the shared line before it, last block first, so each join leaves the
/// lines before it in place. Returns the view and the number of joins.
fn joined(blocks: usize, every: usize) -> (MergeView, tempfile::TempDir, usize) {
    let [left, center, right] = inputs(blocks, "left", "left");
    let (mut view, dir) = open(&left, Some(&center), &right);
    run_until_ready(&mut view);
    let mut joins = 0;
    for block in (0..blocks)
        .rev()
        .filter(|block| every != 0 && block % every == 0)
    {
        let line = u32::try_from(block * BLOCK + BLOCK - 2).unwrap();
        view.output_pane.place(Caret::new(line, 0), false);
        view.output_pane.backspace();
        view.absorb_output_edits();
        joins += 1;
    }
    (view, dir, joins)
}

/// The time one carry of the decisions of `view` onto a new merge of `next`
/// takes, in milliseconds, with the lent records the new model keeps.
fn carry_cost(view: &MergeView, next: &[String; 3]) -> (f64, usize) {
    let (mut fresh, _dir) = open(&next[0], Some(&next[1]), &next[2]);
    run_until_ready(&mut fresh);
    let mut model = fresh.model().clone();
    let started = Instant::now();
    model.carry_over(view.model());
    let spent = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(model.lent_record_problems(), Vec::<String>::new());
    let kept = model
        .sections()
        .iter()
        .map(|section| section.lent_records().len())
        .sum();
    (spent, kept)
}

/// Prints the time the reload pass that carries decisions takes, by file
/// size and by the number of lent records, for the three ways a record
/// passes a reload. Run with `--release --ignored --nocapture`.
#[test]
#[ignore = "a measurement of the reload pass on large files"]
fn the_reload_pass_by_file_size_and_lent_records() {
    for blocks in [1_000, 10_000] {
        let lines = blocks * BLOCK;
        for every in [0, 100, 10, 1] {
            let (view, _dir, joins) = if every == 0 {
                let (mut view, dir, _) = joined(blocks, 0);
                view.current = 1;
                view.run(Command::TakeCenter);
                (view, dir, 0)
            } else {
                joined(blocks, every)
            };
            let same = inputs(blocks, "left", "left");
            let later_line = inputs(blocks, "left", "other");
            let replaced = inputs(blocks, "other", "left");
            let (kept_ms, kept) = carry_cost(&view, &same);
            let (left_out_ms, left_out) = carry_cost(&view, &later_line);
            let (dropped_ms, dropped) = carry_cost(&view, &replaced);
            println!(
                "COST reload lines={lines} joins={joins} unchanged={kept_ms:.1}ms \
                 (records {kept}) lender_later_line_changed={left_out_ms:.1}ms (records \
                 {left_out}) lender_first_line_changed={dropped_ms:.1}ms (records {dropped})"
            );
            assert_eq!(kept, joins);
            assert_eq!(dropped, 0);
        }
    }
}
