//! Unit tests for the debug shell's multi-line-block handling.
//!
//! `next_logical` reads a block written on one command line (`-c $'eval\n…\nend'`)
//! by stripping its terminating `end`, and draws a prompt that names the block
//! for the duration. Both are pure string operations, so they are tested here
//! rather than through a whole session. `#[path]` pulls the file back in as a
//! module of the CLI crate so it can reach the private helpers.

use super::*;

#[test]
fn the_prompt_names_the_open_block() {
    // While `eval` is collecting lines the terminal has to say that the next
    // line is going to `eval`, not to the ordinary command loop.
    assert_eq!(continuation_prompt("eval"), "eval> ");
    assert_eq!(continuation_prompt("commands 1"), "commands 1> ");
    // The terminator is not a prompt, so it is not named after anything.
    assert_ne!(continuation_prompt("eval"), "> ");
}

#[test]
fn an_empty_block_loses_its_terminator() {
    // The newline that separated `eval` from `end` is eaten when the command is
    // split, so the body of an empty block arrives as a bare `end`. It must not
    // survive as AutoIt source or `end` would be evaluated as an expression.
    assert_eq!(strip_block_end("end"), "");
    assert_eq!(strip_block_end("end\n"), "");
    assert_eq!(strip_block_end("end  \n"), "");
    assert_eq!(strip_block_end("END\n"), "");
    assert_eq!(strip_block_end("\tend\r\n"), "");
}

#[test]
fn a_body_keeps_everything_before_the_terminator() {
    assert_eq!(strip_block_end("$x = 1\nend"), "$x = 1\n");
    assert_eq!(strip_block_end("$x = 1\nend\n"), "$x = 1\n");
    assert_eq!(strip_block_end("$x = 1\nend  \n"), "$x = 1\n");
    // Blank interior lines are body, not separators.
    assert_eq!(strip_block_end("$a = 1\n\n$b = 2\nend\n"), "$a = 1\n\n$b = 2\n");
}

#[test]
fn a_block_without_a_terminator_is_left_alone() {
    assert_eq!(strip_block_end("$x = 1"), "$x = 1");
    assert_eq!(strip_block_end("$x = 1\n"), "$x = 1\n");
    // `endless` is an identifier, not the terminator.
    assert_eq!(strip_block_end("endless\n"), "endless\n");
    assert_eq!(strip_block_end("end $x = 1\n"), "end $x = 1\n");
}
