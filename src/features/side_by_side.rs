use itertools::Itertools;
use syntect::highlighting::Style as SyntectStyle;
use unicode_segmentation::UnicodeSegmentation;

use crate::ansi;
use crate::cli;
use crate::config::Config;
use crate::delta::State;
use crate::features::line_numbers;
use crate::features::OptionValueFunction;
use crate::minusplus::*;
use crate::paint::Painter;
use crate::paint::{BgFillMethod, BgShouldFill};
use crate::style::Style;
use crate::wrapping::wrap_zero_block;

pub type LineSegments<'a, S> = Vec<(S, &'a str)>;

pub fn make_feature() -> Vec<(String, OptionValueFunction)> {
    builtin_feature!([
        (
            "side-by-side",
            bool,
            None,
            _opt => true
        ),
        ("features", bool, None, _opt => "line-numbers"),
        ("line-numbers-left-format", String, None, _opt => "│{nm:^4}│".to_string()),
        ("line-numbers-right-format", String, None, _opt => "│{np:^4}│".to_string())
    ])
}

// Aliases for Minus/Plus because Left/Right and PanelSide makes
// more sense in a side-by-side context.
pub use crate::minusplus::MinusPlusIndex as PanelSide;
pub use MinusPlusIndex::Minus as Left;
pub use MinusPlusIndex::Plus as Right;

#[derive(Debug)]
pub struct Panel {
    pub width: usize,
}

pub type LeftRight<T> = MinusPlus<T>;

pub type SideBySideData = LeftRight<Panel>;

impl SideBySideData {
    /// Create a [`LeftRight<Panel>`](LeftRight<Panel>) named [`SideBySideData`].
    pub fn new_sbs(decorations_width: &cli::Width, available_terminal_width: &usize) -> Self {
        let panel_width = match decorations_width {
            cli::Width::Fixed(w) => w / 2,
            _ => available_terminal_width / 2,
        };
        SideBySideData::new(Panel { width: panel_width }, Panel { width: panel_width })
    }
}

pub fn available_line_width(
    config: &Config,
    data: &line_numbers::LineNumbersData,
) -> line_numbers::SideBySideLineWidth {
    let linennumbers_width = data.formatted_width();

    // The width can be reduced by the line numbers and/or a possibly kept 1-wide "+/-/ " prefix.
    let line_width = |side: PanelSide| {
        config.side_by_side_data[side]
            .width
            .saturating_sub(linennumbers_width[side])
            .saturating_sub(config.keep_plus_minus_markers as usize)
    };

    LeftRight::new(line_width(Left), line_width(Right))
}

pub fn line_is_too_long(line: &str, line_width: usize) -> bool {
    let line_sum = line.graphemes(true).count();

    // `line_sum` is too large, because both a leading "+/-/ " and a trailing
    // newline are present, counted, but are never printed. So allow two more
    // characters.
    line_sum > line_width + 2
}

/// Return whether any of the input lines is too long, and a data
/// structure indicating which are too long. This avoids
/// calculating the length again later.
pub fn has_long_lines(
    lines: &LeftRight<&Vec<(String, State)>>,
    line_width: &line_numbers::SideBySideLineWidth,
) -> (bool, LeftRight<Vec<bool>>) {
    let mut wrap_any = LeftRight::default();
    let mut wrapping_lines = LeftRight::default();

    let mut check_if_too_long = |side| {
        let lines_side: &Vec<(String, State)> = lines[side];
        wrapping_lines[side] = lines_side
            .iter()
            .map(|(line, _)| line_is_too_long(line, line_width[side]))
            .inspect(|b| wrap_any[side] |= b)
            .collect();
    };

    check_if_too_long(Left);
    check_if_too_long(Right);

    (wrap_any[Left] || wrap_any[Right], wrapping_lines)
}

/// Emit a sequence of minus and plus lines in side-by-side mode.
#[allow(clippy::too_many_arguments)]
pub fn paint_minus_and_plus_lines_side_by_side<'a>(
    syntax_left_right: LeftRight<Vec<LineSegments<'a, SyntectStyle>>>,
    diff_left_right: LeftRight<Vec<LineSegments<'a, Style>>>,
    states_left_right: LeftRight<Vec<State>>,
    line_alignment: Vec<(Option<usize>, Option<usize>)>,
    output_buffer: &mut String,
    config: &Config,
    line_numbers_data: &mut Option<&mut line_numbers::LineNumbersData>,
    background_color_extends_to_terminal_width: LeftRight<BgShouldFill>,
) {
    for (minus_line_index, plus_line_index) in line_alignment {
        output_buffer.push_str(&paint_left_panel_minus_line(
            minus_line_index,
            &syntax_left_right[Left],
            &diff_left_right[Left],
            match minus_line_index {
                Some(i) => &states_left_right[Left][i],
                None => &State::HunkMinus(None),
            },
            line_numbers_data,
            background_color_extends_to_terminal_width[Left],
            config,
        ));
        output_buffer.push_str(&paint_right_panel_plus_line(
            plus_line_index,
            &syntax_left_right[Right],
            &diff_left_right[Right],
            match plus_line_index {
                Some(i) => &states_left_right[Right][i],
                None => &State::HunkPlus(None),
            },
            line_numbers_data,
            background_color_extends_to_terminal_width[Right],
            config,
        ));
        output_buffer.push('\n');
    }
}

#[allow(clippy::too_many_arguments)]
pub fn paint_zero_lines_side_by_side<'a>(
    raw_line: &str,
    syntax_style_sections: Vec<LineSegments<'a, SyntectStyle>>,
    diff_style_sections: Vec<LineSegments<'a, Style>>,
    output_buffer: &mut String,
    config: &Config,
    line_numbers_data: &mut Option<&mut line_numbers::LineNumbersData>,
    painted_prefix: Option<ansi_term::ANSIString>,
    background_color_extends_to_terminal_width: BgShouldFill,
) {
    let states = vec![State::HunkZero];

    let (states, syntax_style_sections, diff_style_sections) = wrap_zero_block(
        config,
        raw_line,
        states,
        syntax_style_sections,
        diff_style_sections,
        line_numbers_data,
    );

    for (line_index, ((syntax_sections, diff_sections), state)) in syntax_style_sections
        .into_iter()
        .zip_eq(diff_style_sections.iter())
        .zip_eq(states.into_iter())
        .enumerate()
    {
        for panel_side in &[Left, Right] {
            let (mut panel_line, panel_line_is_empty) = Painter::paint_line(
                &syntax_sections,
                diff_sections,
                &state,
                line_numbers_data,
                Some(*panel_side),
                painted_prefix.clone(),
                config,
            );
            pad_panel_line_to_width(
                &mut panel_line,
                panel_line_is_empty,
                Some(line_index),
                &diff_style_sections,
                &state,
                *panel_side,
                background_color_extends_to_terminal_width,
                config,
            );
            output_buffer.push_str(&panel_line);
        }
        output_buffer.push('\n');
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_left_panel_minus_line<'a>(
    line_index: Option<usize>,
    syntax_style_sections: &[LineSegments<'a, SyntectStyle>],
    diff_style_sections: &[LineSegments<'a, Style>],
    state: &'a State,
    line_numbers_data: &mut Option<&mut line_numbers::LineNumbersData>,
    background_color_extends_to_terminal_width: BgShouldFill,
    config: &Config,
) -> String {
    let (mut panel_line, panel_line_is_empty) = paint_minus_or_plus_panel_line(
        line_index,
        syntax_style_sections,
        diff_style_sections,
        state,
        line_numbers_data,
        Left,
        config,
    );
    pad_panel_line_to_width(
        &mut panel_line,
        panel_line_is_empty,
        line_index,
        diff_style_sections,
        state,
        Left,
        background_color_extends_to_terminal_width,
        config,
    );

    panel_line
}

#[allow(clippy::too_many_arguments)]
fn paint_right_panel_plus_line<'a>(
    line_index: Option<usize>,
    syntax_style_sections: &[LineSegments<'a, SyntectStyle>],
    diff_style_sections: &[LineSegments<'a, Style>],
    state: &'a State,
    line_numbers_data: &mut Option<&mut line_numbers::LineNumbersData>,
    background_color_extends_to_terminal_width: BgShouldFill,
    config: &Config,
) -> String {
    let (mut panel_line, panel_line_is_empty) = paint_minus_or_plus_panel_line(
        line_index,
        syntax_style_sections,
        diff_style_sections,
        state,
        line_numbers_data,
        Right,
        config,
    );

    pad_panel_line_to_width(
        &mut panel_line,
        panel_line_is_empty,
        line_index,
        diff_style_sections,
        state,
        Right,
        background_color_extends_to_terminal_width,
        config,
    );

    panel_line
}

fn get_right_fill_style_for_panel<'a>(
    line_is_empty: bool,
    line_index: Option<usize>,
    diff_style_sections: &[LineSegments<'a, Style>],
    state: &State,
    panel_side: PanelSide,
    background_color_extends_to_terminal_width: BgShouldFill,
    config: &Config,
) -> (Option<BgFillMethod>, Style) {
    // If in the the left panel then it must be filled with spaces.
    let none_or_override = if panel_side == Left {
        Some(BgFillMethod::Spaces)
    } else {
        None
    };

    match (line_is_empty, line_index) {
        (true, _) => (none_or_override, config.null_style),
        (false, None) => (none_or_override, config.null_style),
        (false, Some(index)) => {
            let (bg_fill_mode, fill_style) =
                Painter::get_should_right_fill_background_color_and_fill_style(
                    &diff_style_sections[index],
                    state,
                    background_color_extends_to_terminal_width,
                    config,
                );

            match bg_fill_mode {
                None => (none_or_override, config.null_style),
                _ if panel_side == Left => (Some(BgFillMethod::Spaces), fill_style),
                _ => (bg_fill_mode, fill_style),
            }
        }
    }
}

/// Construct half of a minus or plus line under side-by-side mode, i.e. the half line that
/// goes in one or other panel. Return a tuple `(painted_half_line, is_empty)`.
// Suppose the line being displayed is a minus line with a paired plus line. Then both times
// this function is called, `line_index` will be `Some`. This case proceeds as one would
// expect: on the first call, we are constructing the left panel line, and we are passed
// `(Some(index), HunkMinus, Left)`. We pass `(HunkMinus, Left)` to
// `paint_line`. This has two consequences:
// 1. `format_and_paint_line_numbers` will increment the minus line number.
// 2. `format_and_paint_line_numbers` will emit the left line number field, and not the right.
//
// The second call does the analogous thing for the plus line to be displayed in the right panel:
// we are passed `(Some(index), HunkPlus, Right)` and we pass `(HunkPlus, Right)` to `paint_line`,
// causing it to increment the plus line number and emit the right line number field.
//
// Now consider the case where the line being displayed is a minus line with no paired plus line.
// The first call is as before. On the second call, we are passed `(None, HunkPlus, Right)` and we
// wish to display the right panel, with its line number container, but without any line number
// (and without any line contents). We do this by passing (HunkMinus, Right) to `paint_line`, since
// what this will do is set the line number pair in that function to `(Some(minus_number), None)`,
// and then only emit the right field (which has a None number, i.e. blank). However, it will also
// increment the minus line number, so we need to knock that back down.
#[allow(clippy::too_many_arguments)]
fn paint_minus_or_plus_panel_line<'a>(
    line_index: Option<usize>,
    syntax_style_sections: &[LineSegments<'a, SyntectStyle>],
    diff_style_sections: &[LineSegments<'a, Style>],
    state: &State,
    line_numbers_data: &mut Option<&mut line_numbers::LineNumbersData>,
    panel_side: PanelSide,
    config: &Config,
) -> (String, bool) {
    let (empty_line_syntax_sections, empty_line_diff_sections) = (Vec::new(), Vec::new());

    let (line_syntax_sections, line_diff_sections, state_for_line_numbers_field) =
        if let Some(index) = line_index {
            (
                &syntax_style_sections[index],
                &diff_style_sections[index],
                state.clone(),
            )
        } else {
            let opposite_state = match state {
                State::HunkMinus(x) => State::HunkPlus(x.clone()),
                State::HunkPlus(x) => State::HunkMinus(x.clone()),
                _ => unreachable!(),
            };
            (
                &empty_line_syntax_sections,
                &empty_line_diff_sections,
                opposite_state,
            )
        };

    let painted_prefix = match (config.keep_plus_minus_markers, panel_side, state) {
        (true, _, State::HunkPlusWrapped) => Some(config.plus_style.paint(" ")),
        (true, _, State::HunkMinusWrapped) => Some(config.minus_style.paint(" ")),
        (true, Left, _) => Some(config.minus_style.paint("-")),
        (true, Right, _) => Some(config.plus_style.paint("+")),
        _ => None,
    };

    let (line, line_is_empty) = Painter::paint_line(
        line_syntax_sections,
        line_diff_sections,
        &state_for_line_numbers_field,
        line_numbers_data,
        Some(panel_side),
        painted_prefix,
        config,
    );

    (line, line_is_empty)
}

/// Right-fill the background color of a line in a panel. If in the left panel this is always
/// done with spaces. The right panel can be filled with spaces or using ANSI sequences
/// instructing the terminal emulator to fill the background color rightwards.
#[allow(clippy::too_many_arguments, clippy::comparison_chain)]
fn pad_panel_line_to_width<'a>(
    panel_line: &mut String,
    panel_line_is_empty: bool,
    line_index: Option<usize>,
    diff_style_sections: &[LineSegments<'a, Style>],
    state: &State,
    panel_side: PanelSide,
    background_color_extends_to_terminal_width: BgShouldFill,
    config: &Config,
) {
    // Emit empty line marker if the panel line is empty but not empty-by-construction. IOW if the
    // other panel contains a real line, and we are currently emitting an empty counterpart panel
    // to form the other half of the line, then don't emit the empty line marker.
    if panel_line_is_empty && line_index.is_some() {
        match state {
            State::HunkMinus(_) => Painter::mark_empty_line(
                &config.minus_empty_line_marker_style,
                panel_line,
                Some(" "),
            ),
            State::HunkPlus(_) => Painter::mark_empty_line(
                &config.plus_empty_line_marker_style,
                panel_line,
                Some(" "),
            ),
            State::HunkZero => {}
            _ => unreachable!(),
        };
    };

    let text_width = ansi::measure_text_width(panel_line);
    let panel_width = config.side_by_side_data[panel_side].width;

    if text_width > panel_width {
        *panel_line =
            ansi::truncate_str(panel_line, panel_width, &config.truncation_symbol).to_string();
    }

    let (bg_fill_mode, fill_style) = get_right_fill_style_for_panel(
        panel_line_is_empty,
        line_index,
        diff_style_sections,
        state,
        panel_side,
        background_color_extends_to_terminal_width,
        config,
    );

    match bg_fill_mode {
        Some(BgFillMethod::TryAnsiSequence) => {
            Painter::right_fill_background_color(panel_line, fill_style)
        }
        Some(BgFillMethod::Spaces) if text_width >= panel_width => (),
        Some(BgFillMethod::Spaces) => panel_line.push_str(
            &fill_style
                .paint(" ".repeat(panel_width - text_width))
                .to_string(),
        ),
        None => (),
    }
}

pub mod ansifill {
    use super::SideBySideData;
    use crate::config::Config;
    use crate::paint::BgFillMethod;

    pub const ODD_PAD_CHAR: char = ' ';

    // Panels in side-by-side mode always sum up to an even number, so when the terminal
    // has an odd width an extra column is left over.
    // If the background color is extended with an ANSI sequence (which only knows "fill
    // this row until the end") instead of spaces (see `BgFillMethod`), then the coloring
    // extends into that column. This becomes noticeable when the displayed content reaches
    // the right side of the right panel to be truncated or wrapped.
    // However using an ANSI sequence instead of spaces is generally preferable because
    // small changes to the terminal width are less noticeable.

    /// The solution in this case is to add `ODD_PAD_CHAR` before the first line number in
    /// the right panel and increasing its width by one, thus using the full terminal width
    /// with the two panels.
    /// This also means line numbers can not be disabled in side-by-side mode, but they may
    /// not actually paint numbers.
    #[derive(Clone, Debug)]
    pub struct UseFullPanelWidth(pub bool);
    impl UseFullPanelWidth {
        pub fn new(config: &Config) -> Self {
            Self(
                config.side_by_side
                    && Self::is_odd_with_ansi(&config.decorations_width, &config.line_fill_method),
            )
        }
        pub fn sbs_odd_fix(
            width: &crate::cli::Width,
            method: &BgFillMethod,
            sbs_data: SideBySideData,
        ) -> SideBySideData {
            if Self::is_odd_with_ansi(width, method) {
                Self::adapt_sbs_data(sbs_data)
            } else {
                sbs_data
            }
        }
        pub fn pad_width(&self) -> bool {
            self.0
        }
        fn is_odd_with_ansi(width: &crate::cli::Width, method: &BgFillMethod) -> bool {
            method == &BgFillMethod::TryAnsiSequence
                && matches!(&width, crate::cli::Width::Fixed(width) if width % 2 == 1)
        }
        fn adapt_sbs_data(mut sbs_data: SideBySideData) -> SideBySideData {
            sbs_data[super::Right].width += 1;
            sbs_data
        }
    }
}

#[cfg(test)]
pub mod tests {
    use crate::ansi::strip_ansi_codes;
    use crate::features::line_numbers::tests::*;
    use crate::tests::integration_test_utils::{make_config_from_args, run_delta};

    #[test]
    fn test_two_minus_lines() {
        let config = make_config_from_args(&["--side-by-side", "--width", "40"]);
        let output = run_delta(TWO_MINUS_LINES_DIFF, &config);
        let mut lines = output.lines().skip(crate::config::HEADER_LEN);
        let (line_1, line_2) = (lines.next().unwrap(), lines.next().unwrap());
        assert_eq!("│ 1  │a = 1         │    │", strip_ansi_codes(line_1));
        assert_eq!("│ 2  │b = 23456     │    │", strip_ansi_codes(line_2));
    }

    #[test]
    fn test_two_minus_lines_truncated() {
        let mut config = make_config_from_args(&[
            "--side-by-side",
            "--wrap-max-lines",
            "0",
            "--width",
            "28",
            "--line-fill-method=spaces",
        ]);
        config.truncation_symbol = ">".into();
        let output = run_delta(TWO_MINUS_LINES_DIFF, &config);
        let mut lines = output.lines().skip(crate::config::HEADER_LEN);
        let (line_1, line_2) = (lines.next().unwrap(), lines.next().unwrap());
        assert_eq!("│ 1  │a = 1   │    │", strip_ansi_codes(line_1));
        assert_eq!("│ 2  │b = 234>│    │", strip_ansi_codes(line_2));
    }

    #[test]
    fn test_two_plus_lines() {
        let config = make_config_from_args(&[
            "--side-by-side",
            "--width",
            "41",
            "--line-fill-method=spaces",
        ]);
        let output = run_delta(TWO_PLUS_LINES_DIFF, &config);
        let mut lines = output.lines().skip(crate::config::HEADER_LEN);
        let (line_1, line_2) = (lines.next().unwrap(), lines.next().unwrap());
        let sac = strip_ansi_codes; // alias to help with `cargo fmt`-ing:
        assert_eq!("│    │              │ 1  │a = 1         ", sac(line_1));
        assert_eq!("│    │              │ 2  │b = 234567    ", sac(line_2));
    }

    #[test]
    fn test_two_plus_lines_truncated() {
        let mut config = make_config_from_args(&[
            "--side-by-side",
            "--wrap-max-lines",
            "0",
            "--width",
            "30",
            "--line-fill-method=spaces",
        ]);
        config.truncation_symbol = ">".into();

        let output = run_delta(TWO_PLUS_LINES_DIFF, &config);
        let mut lines = output.lines().skip(crate::config::HEADER_LEN);
        let (line_1, line_2) = (lines.next().unwrap(), lines.next().unwrap());
        assert_eq!("│    │         │ 1  │a = 1    ", strip_ansi_codes(line_1));
        assert_eq!("│    │         │ 2  │b = 2345>", strip_ansi_codes(line_2));
    }

    #[test]
    fn test_two_plus_lines_exact_fit() {
        let config =
            make_config_from_args(&["--side-by-side", "--width", "33", "--line-fill-method=ansi"]);
        let output = run_delta(TWO_PLUS_LINES_DIFF, &config);
        let mut lines = output.lines().skip(crate::config::HEADER_LEN);
        let (line_1, line_2) = (lines.next().unwrap(), lines.next().unwrap());
        let sac = strip_ansi_codes; // alias to help with `cargo fmt`-ing:
        assert_eq!("│    │           │ 1  │a = 1", sac(line_1));
        assert_eq!("│    │           │ 2  │b = 234567", sac(line_2));
    }

    #[test]
    fn test_one_minus_one_plus_line() {
        let config = make_config_from_args(&[
            "--side-by-side",
            "--width",
            "40",
            "--line-fill-method=spaces",
        ]);
        let output = run_delta(ONE_MINUS_ONE_PLUS_LINE_DIFF, &config);
        let output = strip_ansi_codes(&output);
        let mut lines = output.lines().skip(crate::config::HEADER_LEN);
        let mut lnu = move || lines.next().unwrap(); // for cargo fmt
        assert_eq!("│ 1  │a = 1         │ 1  │a = 1", lnu());
        assert_eq!("│ 2  │b = 2         │ 2  │bb = 2        ", lnu());
    }
}
