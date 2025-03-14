use syntect::highlighting::Style as SyntectStyle;
use unicode_segmentation::UnicodeSegmentation;

use crate::config::INLINE_SYMBOL_WIDTH_1;

use crate::config::Config;
use crate::delta::State;
use crate::features::line_numbers;
use crate::features::line_numbers::SideBySideLineWidth;
use crate::features::side_by_side::available_line_width;
use crate::features::side_by_side::line_is_too_long;
use crate::features::side_by_side::LineSegments;
use crate::features::side_by_side::{Left, Right};
use crate::minusplus::*;
use crate::style::Style;

/// See [`wrap_line`] for documentation.
#[derive(Clone, Debug)]
pub struct WrapConfig {
    pub left_symbol: String,
    pub right_symbol: String,
    pub right_prefix_symbol: String,
    // In fractions of 1000 so that a >100 wide panel can
    // still be configured down to a single character.
    pub use_wrap_right_permille: usize,
    // This value is --wrap-max-lines + 1, and unlimited is 0, see
    // adapt_wrap_max_lines_argument()
    pub max_lines: usize,
    pub inline_hint_syntect_style: SyntectStyle,
}

/// Wrap the given `line` if it is longer than `line_width`. Wrap to at most
/// [Config::WrapConfig::max_lines](WrapConfig::max_lines) lines,
/// then truncate again - but never truncate if it is `0`. Place
/// [left_symbol](WrapConfig::left_symbol) at the end of wrapped lines.
/// If wrapping results in only *one* extra line and if the width of the wrapped
/// line is less than [use_wrap_right_permille](WrapConfig::use_wrap_right_permille)
/// then right-align the second line and use the symbols
/// [right_symbol](WrapConfig::right_symbol) and
/// on the next line [right_prefix_symbol](WrapConfig::right_prefix_symbol).
/// The inserted characters will follow the
/// [inline_hint_syntect_style](WrapConfig::inline_hint_syntect_style).
///
/// The input `line` is expected to start with an (ultimately not printed) `+`, `-` or ` ` prefix.
/// The prefix `_` is also added to the start of wrapped lines.
pub fn wrap_line<'a, I, S>(
    config: &'a Config,
    line: I,
    line_width: usize,
    fill_style: &S,
    inline_hint_style: &Option<S>,
) -> Vec<LineSegments<'a, S>>
where
    I: IntoIterator<Item = (S, &'a str)> + std::fmt::Debug,
    <I as IntoIterator>::IntoIter: DoubleEndedIterator,
    S: Copy + Default + std::fmt::Debug,
{
    let mut result = Vec::new();

    let wrap_config = &config.wrap_config;

    // Symbol which:
    //  - represents the additional "+/-/ " prefix on the unwrapped input line, its
    //    length is added to the line_width.
    //  - can be more prominent than a space because syntax highlighting has already
    //    been done.
    //  - is added at the beginning of wrapped lines so the wrapped lines also have
    //    a prefix (which is not printed).
    const LINEPREFIX: &str = "_";
    assert_eq!(LINEPREFIX.len(), INLINE_SYMBOL_WIDTH_1); // (args are const, optimized out)

    let max_len = line_width + LINEPREFIX.len();

    // The current line being assembled from the input to fit exactly into the given width.
    // A somewhat leaky abstraction as the fields are also accessed directly.
    struct CurrLine<'a, S: Default> {
        line_segments: LineSegments<'a, S>,
        len: usize,
    }
    impl<'a, S: Default> CurrLine<'a, S> {
        fn reset() -> Self {
            CurrLine {
                line_segments: vec![(S::default(), LINEPREFIX)],
                len: LINEPREFIX.len(),
            }
        }
        fn push_and_set_len(&mut self, text: (S, &'a str), len: usize) {
            self.line_segments.push(text);
            self.len = len;
        }
        fn has_text(&self) -> bool {
            self.len > LINEPREFIX.len()
        }
        fn text_len(&self) -> usize {
            if self.len > LINEPREFIX.len() {
                self.len - LINEPREFIX.len()
            } else {
                debug_assert!(false, "push or reset first");
                0
            }
        }
    }

    // The first `push_and_set_len` will include the "+/-/ " prefix, subsequent
    // `reset()` add `LINEPREFIX`. Thus each line starts with a prefix.
    let mut curr_line: CurrLine<S> = CurrLine {
        line_segments: Vec::new(),
        len: 0,
    };

    // Determine the background (diff) and color (syntax) of an inserted symbol.
    let symbol_style = match inline_hint_style {
        Some(style) => *style,
        None => *fill_style,
    };

    let mut stack = line.into_iter().rev().collect::<Vec<_>>();

    let line_limit_reached = |result: &Vec<_>| {
        // If only the wrap symbol and no extra text fits, then wrapping is not possible.
        let max_lines = if line_width <= INLINE_SYMBOL_WIDTH_1 {
            1
        } else {
            wrap_config.max_lines
        };

        max_lines > 0 && result.len() + 1 >= max_lines
    };

    while !stack.is_empty() && !line_limit_reached(&result) && max_len > LINEPREFIX.len() {
        let (style, text, graphemes) = stack
            .pop()
            .map(|(style, text)| (style, text, text.grapheme_indices(true).collect::<Vec<_>>()))
            .unwrap();

        let new_len = curr_line.len + graphemes.len();

        let must_split = if new_len < max_len {
            curr_line.push_and_set_len((style, text), new_len);
            false
        } else if new_len == max_len {
            match stack.last() {
                // Perfect fit, no need to make space for a `wrap_symbol`.
                None => {
                    curr_line.push_and_set_len((style, text), new_len);
                    false
                }
                #[allow(clippy::identity_op)]
                // A single '\n' left on the stack can be pushed onto the current line.
                Some((next_style, nl)) if stack.len() == 1 && *nl == "\n" => {
                    curr_line.push_and_set_len((style, text), new_len);
                    // Do not count the '\n': + 0
                    curr_line.push_and_set_len((*next_style, *nl), new_len + 0);
                    stack.pop();
                    false
                }
                _ => true,
            }
        } else if new_len == max_len + 1 && stack.is_empty() {
            // If the one overhanging char is '\n' then keep it on the current line.
            if text.ends_with('\n') {
                // Do not count the included '\n': - 1
                curr_line.push_and_set_len((style, text), new_len - 1);
                false
            } else {
                true
            }
        } else {
            true
        };

        // Text must be split, one part (or just `wrap_symbol`) is added to the
        // current line, the other is pushed onto the stack.
        if must_split {
            let grapheme_split_pos = graphemes.len() - (new_len - max_len) - 1;

            // The length does not matter anymore and `curr_line` will be reset
            // at the end, so move the line segments out.
            let mut line_segments = curr_line.line_segments;

            let next_line = if grapheme_split_pos == 0 {
                text
            } else {
                let byte_split_pos = graphemes[grapheme_split_pos].0;
                let this_line = &text[..byte_split_pos];
                line_segments.push((style, this_line));
                &text[byte_split_pos..]
            };
            stack.push((style, next_line));

            line_segments.push((symbol_style, &wrap_config.left_symbol));
            result.push(line_segments);

            curr_line = CurrLine::reset();
        }
    }

    // Right-align wrapped line:
    // Done if wrapping adds exactly one line and this line is less than the given
    // permille wide. Also change the wrap symbol at the end of the previous (first) line.
    if result.len() == 1 && curr_line.has_text() {
        let current_permille = (curr_line.text_len() * 1000) / max_len;

        let pad_len = max_len.saturating_sub(curr_line.text_len() + INLINE_SYMBOL_WIDTH_1);

        if wrap_config.use_wrap_right_permille > current_permille && pad_len > INLINE_SYMBOL_WIDTH_1
        {
            // The inserted spaces, which align a line to the right, point into this string.
            const SPACES: &str = "                                                                ";

            match result.last_mut() {
                Some(ref mut vec) if !vec.is_empty() => {
                    vec.last_mut().unwrap().1 = &wrap_config.right_symbol
                }
                _ => unreachable!("wrap result must not be empty"),
            }

            let mut right_aligned_line = vec![(S::default(), LINEPREFIX)];

            for _ in 0..(pad_len / SPACES.len()) {
                right_aligned_line.push((*fill_style, SPACES));
            }

            match pad_len % SPACES.len() {
                0 => (),
                n => right_aligned_line.push((*fill_style, &SPACES[0..n])),
            }

            right_aligned_line.push((symbol_style, &wrap_config.right_prefix_symbol));

            // skip LINEPREFIX which `CurrLine::reset()` adds
            right_aligned_line.extend(curr_line.line_segments.into_iter().skip(1));

            curr_line.line_segments = right_aligned_line;

            // curr_line.len not updated, as only 0 / 1 / > 1 is required now
        }
    }

    if curr_line.len > 0 {
        result.push(curr_line.line_segments);
    }

    // Anything that is left will be added to the (last) line. If this is too long it will
    // be truncated later.
    if !stack.is_empty() {
        if result.is_empty() {
            result.push(Vec::new());
        }

        // unwrap: previous `if` ensures result can not be empty
        result.last_mut().unwrap().extend(stack.into_iter().rev());
    }

    result
}

fn wrap_if_too_long<'a, S>(
    config: &'a Config,
    wrapped: &mut Vec<LineSegments<'a, S>>,
    input_vec: LineSegments<'a, S>,
    must_wrap: bool,
    line_width: usize,
    fill_style: &S,
    inline_hint_style: &Option<S>,
) -> (usize, usize)
where
    S: Copy + Default + std::fmt::Debug,
{
    let size_prev = wrapped.len();

    if must_wrap {
        wrapped.append(&mut wrap_line(
            config,
            input_vec.into_iter(),
            line_width,
            fill_style,
            inline_hint_style,
        ));
    } else {
        wrapped.push(input_vec.to_vec());
    }

    (size_prev, wrapped.len())
}

/// Call [`wrap_line`] for the `syntax` and the `diff` lines if `wrapinfo` says
/// a specific line was longer than `line_width`. Return an adjusted `alignment`
/// with regard to the added wrapped lines.
#[allow(clippy::comparison_chain, clippy::type_complexity)]
pub fn wrap_minusplus_block<'c: 'a, 'a>(
    config: &'c Config,
    syntax: MinusPlus<Vec<LineSegments<'a, SyntectStyle>>>,
    diff: MinusPlus<Vec<LineSegments<'a, Style>>>,
    alignment: &[(Option<usize>, Option<usize>)],
    line_width: &SideBySideLineWidth,
    wrapinfo: &'a MinusPlus<Vec<bool>>,
) -> (
    Vec<(Option<usize>, Option<usize>)>,
    MinusPlus<Vec<State>>,
    MinusPlus<Vec<LineSegments<'a, SyntectStyle>>>,
    MinusPlus<Vec<LineSegments<'a, Style>>>,
) {
    let mut new_alignment = Vec::new();
    let mut new_states = MinusPlus::<Vec<State>>::default();
    let mut new_wrapped_syntax = MinusPlus::default();
    let mut new_wrapped_diff = MinusPlus::default();

    // Turn all these into pairs of iterators so they can be advanced according
    // to the alignment and independently.
    let mut syntax = MinusPlus::new(syntax.minus.into_iter(), syntax.plus.into_iter());
    let mut diff = MinusPlus::new(diff.minus.into_iter(), diff.plus.into_iter());
    let mut wrapinfo = MinusPlus::new(wrapinfo[Left].iter(), wrapinfo[Right].iter());

    let fill_style = MinusPlus::new(&config.minus_style, &config.plus_style);

    // Internal helper function to perform wrapping for both the syntax and the
    // diff highlighting (SyntectStyle and Style).
    #[allow(clippy::too_many_arguments)]
    pub fn wrap_syntax_and_diff<'a, ItSyn, ItDiff, ItWrap>(
        config: &'a Config,
        wrapped_syntax: &mut Vec<LineSegments<'a, SyntectStyle>>,
        wrapped_diff: &mut Vec<LineSegments<'a, Style>>,
        syntax_iter: &mut ItSyn,
        diff_iter: &mut ItDiff,
        wrapinfo_iter: &mut ItWrap,
        line_width: usize,
        fill_style: &Style,
        errhint: &'a str,
    ) -> (usize, usize)
    where
        ItSyn: Iterator<Item = LineSegments<'a, SyntectStyle>>,
        ItDiff: Iterator<Item = LineSegments<'a, Style>>,
        ItWrap: Iterator<Item = &'a bool>,
    {
        let must_wrap = *wrapinfo_iter
            .next()
            .unwrap_or_else(|| panic!("bad wrap info {}", errhint));

        let (start, extended_to) = wrap_if_too_long(
            config,
            wrapped_syntax,
            syntax_iter
                .next()
                .unwrap_or_else(|| panic!("bad syntax alignment {}", errhint)),
            must_wrap,
            line_width,
            &config.null_syntect_style,
            &Some(config.wrap_config.inline_hint_syntect_style),
        );

        // TODO: Why is the background color set to white when
        // ansi_term_style.background is None?
        let inline_hint_style = if config
            .inline_hint_style
            .ansi_term_style
            .background
            .is_some()
        {
            Some(config.inline_hint_style)
        } else {
            None
        };

        let (start2, extended_to2) = wrap_if_too_long(
            config,
            wrapped_diff,
            diff_iter
                .next()
                .unwrap_or_else(|| panic!("bad diff alignment {}", errhint)),
            must_wrap,
            line_width,
            fill_style,
            &inline_hint_style,
        );

        // The underlying text is the same for the style and diff, so
        // the length of the wrapping should be identical:
        assert_eq!(
            (start, extended_to),
            (start2, extended_to2),
            "syntax and diff wrapping differs {}",
            errhint
        );

        (start, extended_to)
    }

    // This macro avoids having the same code block 4x in the alignment processing
    macro_rules! wrap_and_assert {
        ($side:tt, $errhint:tt, $have:tt, $expected:tt) => {{
            assert_eq!(*$have, $expected, "bad alignment index {}", $errhint);
            $expected += 1;

            wrap_syntax_and_diff(
                &config,
                &mut new_wrapped_syntax[$side],
                &mut new_wrapped_diff[$side],
                &mut syntax[$side],
                &mut diff[$side],
                &mut wrapinfo[$side],
                line_width[$side],
                &fill_style[$side],
                $errhint,
            )
        }};
    }

    let mut m_expected = 0;
    let mut p_expected = 0;

    // Process blocks according to the alignment and build a new alignment.
    // If lines get added via wrapping these are assigned the state HunkMinusWrapped/HunkPlusWrapped.
    for (minus, plus) in alignment {
        let (minus_extended, plus_extended) = match (minus, plus) {
            (Some(m), None) => {
                let (minus_start, extended_to) = wrap_and_assert!(Left, "[*l*] (-)", m, m_expected);

                for i in minus_start..extended_to {
                    new_alignment.push((Some(i), None));
                }

                (extended_to - minus_start, 0)
            }
            (None, Some(p)) => {
                let (plus_start, extended_to) = wrap_and_assert!(Right, "(-) [*r*]", p, p_expected);

                for i in plus_start..extended_to {
                    new_alignment.push((None, Some(i)));
                }

                (0, extended_to - plus_start)
            }
            (Some(m), Some(p)) => {
                let (minus_start, m_extended_to) =
                    wrap_and_assert!(Left, "[*l*] (r)", m, m_expected);
                let (plus_start, p_extended_to) =
                    wrap_and_assert!(Right, "(l) [*r*]", p, p_expected);

                for (new_m, new_p) in (minus_start..m_extended_to).zip(plus_start..p_extended_to) {
                    new_alignment.push((Some(new_m), Some(new_p)));
                }

                // This Some(m):Some(p) alignment might have become uneven, so fill
                // up the shorter side with None.

                let minus_extended = m_extended_to - minus_start;
                let plus_extended = p_extended_to - plus_start;

                let plus_minus = (minus_extended as isize) - (plus_extended as isize);

                if plus_minus > 0 {
                    for m in (m_extended_to as isize - plus_minus) as usize..m_extended_to {
                        new_alignment.push((Some(m), None));
                    }
                } else if plus_minus < 0 {
                    for p in (p_extended_to as isize + plus_minus) as usize..p_extended_to {
                        new_alignment.push((None, Some(p)));
                    }
                }

                (minus_extended, plus_extended)
            }
            _ => unreachable!("None-None alignment"),
        };

        if minus_extended > 0 {
            new_states[Left].push(State::HunkMinus(None));
            for _ in 1..minus_extended {
                new_states[Left].push(State::HunkMinusWrapped);
            }
        }
        if plus_extended > 0 {
            new_states[Right].push(State::HunkPlus(None));
            for _ in 1..plus_extended {
                new_states[Right].push(State::HunkPlusWrapped);
            }
        }
    }

    (
        new_alignment,
        new_states,
        new_wrapped_syntax,
        new_wrapped_diff,
    )
}

#[allow(clippy::comparison_chain, clippy::type_complexity)]
pub fn wrap_zero_block<'c: 'a, 'a>(
    config: &'c Config,
    raw_line: &str,
    mut states: Vec<State>,
    syntax_style_sections: Vec<LineSegments<'a, SyntectStyle>>,
    diff_style_sections: Vec<LineSegments<'a, Style>>,
    line_numbers_data: &Option<&mut line_numbers::LineNumbersData>,
) -> (
    Vec<State>,
    Vec<LineSegments<'a, SyntectStyle>>,
    Vec<LineSegments<'a, Style>>,
) {
    // The width is the minimum of the left/right side. The panels should be equally sized,
    // but in rare cases the remaining panel width might differ due to the space the line
    // numbers take up.
    let line_width = if let Some(line_numbers_data) = line_numbers_data {
        let width = available_line_width(config, line_numbers_data);
        std::cmp::min(width[Left], width[Right])
    } else {
        std::cmp::min(
            config.side_by_side_data[Left].width,
            config.side_by_side_data[Right].width,
        )
    };

    // Called with a single line, so no need to use the 1-sized bool vector.
    // If that changes the wrapping logic should be updated as well.
    debug_assert_eq!(diff_style_sections.len(), 1);

    let should_wrap = line_is_too_long(raw_line, line_width);

    if should_wrap {
        let syntax_style = wrap_line(
            config,
            syntax_style_sections.into_iter().flatten(),
            line_width,
            &SyntectStyle::default(),
            &Some(config.wrap_config.inline_hint_syntect_style),
        );

        // TODO: Why is the background color set to white when
        // ansi_term_style.background is None?
        let inline_hint_style = if config
            .inline_hint_style
            .ansi_term_style
            .background
            .is_some()
        {
            Some(config.inline_hint_style)
        } else {
            None
        };
        let diff_style = wrap_line(
            config,
            diff_style_sections.into_iter().flatten(),
            line_width,
            // To actually highlight inline hint characters:
            &Style {
                is_syntax_highlighted: true,
                ..config.null_style
            },
            &inline_hint_style,
        );

        states.resize_with(syntax_style.len(), || State::HunkZeroWrapped);

        (states, syntax_style, diff_style)
    } else {
        (states, syntax_style_sections, diff_style_sections)
    }
}

#[cfg(test)]
mod tests {
    use lazy_static::lazy_static;
    use syntect::highlighting::Style as SyntectStyle;

    use super::wrap_line;
    use super::WrapConfig;
    use crate::ansi::strip_ansi_codes;
    use crate::config::Config;
    use crate::features::side_by_side::LineSegments;
    use crate::style::Style;
    use crate::tests::integration_test_utils::{make_config_from_args, run_delta};

    lazy_static! {
        static ref S1: Style = Style {
            is_syntax_highlighted: true,
            ..Default::default()
        };
    }
    lazy_static! {
        static ref S2: Style = Style {
            is_emph: true,
            ..Default::default()
        };
    }
    lazy_static! {
        static ref SY: SyntectStyle = SyntectStyle::default();
    }
    lazy_static! {
        static ref SD: Style = Style::default();
    }

    const W: &str = &"+"; // wrap
    const WR: &str = &"<"; // wrap-right
    const RA: &str = &">"; // right-align

    lazy_static! {
        static ref WRAP_DEFAULT_ARGS: Vec<&'static str> = vec![
            "--wrap-left-symbol",
            W,
            "--wrap-right-symbol",
            WR,
            "--wrap-right-prefix-symbol",
            RA,
            "--wrap-max-lines",
            "4",
            "--wrap-right-percent",
            "37.0%",
        ];
    }

    lazy_static! {
        static ref TEST_WRAP_CFG: WrapConfig =
            make_config_from_args(&WRAP_DEFAULT_ARGS).wrap_config;
    }

    fn default_wrap_cfg_plus<'a>(args: &[&'a str]) -> Vec<&'a str> {
        let mut result = WRAP_DEFAULT_ARGS.clone();
        result.extend_from_slice(args);
        result
    }

    fn mk_wrap_cfg(wrap_cfg: &WrapConfig) -> Config {
        let mut cfg: Config = Config::from(make_config_from_args(&[]));
        cfg.wrap_config = wrap_cfg.clone();
        cfg
    }

    fn wrap_test<'a, I, S>(cfg: &'a Config, line: I, line_width: usize) -> Vec<LineSegments<'a, S>>
    where
        I: IntoIterator<Item = (S, &'a str)> + std::fmt::Debug,
        <I as IntoIterator>::IntoIter: DoubleEndedIterator,
        S: Copy + Default + std::fmt::Debug,
    {
        wrap_line(&cfg, line, line_width, &S::default(), &None)
    }

    #[test]
    fn test_wrap_line_single() {
        let cfg = mk_wrap_cfg(&TEST_WRAP_CFG);

        {
            // Empty input without a "+/-/ "-prefix usually does not happen
            let line = vec![(*S1, "")];
            let lines = wrap_test(&cfg, line, 6);
            assert!(lines.is_empty());
        }

        {
            let line = vec![(*SY, "_0")];
            let lines = wrap_test(&cfg, line, 6);
            assert_eq!(lines, vec![vec![(*SY, "_0")]]);
        }

        {
            let line = vec![(*S1, "_")];
            let lines = wrap_test(&cfg, line, 6);
            assert_eq!(lines, vec![vec![(*S1, "_")]]);
        }

        {
            let line = vec![(*S1, "_"), (*S2, "0")];
            let lines = wrap_test(&cfg, line, 6);
            assert_eq!(lines, vec![vec![(*S1, "_"), (*S2, "0")]]);
        }

        {
            let line = vec![(*S1, "_012"), (*S2, "34")];
            let lines = wrap_test(&cfg, line, 6);
            assert_eq!(lines, vec![vec![(*S1, "_012"), (*S2, "34")]]);
        }

        {
            let line = vec![(*S1, "_012"), (*S2, "345")];
            let lines = wrap_test(&cfg, line, 6);
            assert_eq!(lines, vec![vec![(*S1, "_012"), (*S2, "345")]]);
        }
    }

    #[test]
    fn test_wrap_line_align_right() {
        let cfg = mk_wrap_cfg(&TEST_WRAP_CFG);

        let line = vec![(*S1, "_0123456789ab")];
        let lines = wrap_test(&cfg, line, 11);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].last().unwrap().1, WR);
        assert_eq!(
            lines[1],
            vec![(*SD, "_"), (*SD, "         "), (*SD, ">"), (*S1, "ab")]
        );
    }

    #[test]
    fn test_wrap_line_align_right_2() {
        let line = vec![(*S1, "_012"), (*S2, "3456")];

        {
            // Right align lines on the second line
            let cfg = mk_wrap_cfg(&TEST_WRAP_CFG);

            let lines = wrap_test(&cfg, line.clone(), 6);
            assert_eq!(
                lines,
                vec![
                    vec![(*S1, "_012"), (*S2, "34"), (*SD, WR)],
                    vec![(*SD, "_"), (*SD, "    "), (*SD, RA), (*S2, "56")]
                ]
            );
        }

        {
            // Set right align percentage lower, normal wrapping
            let mut no_align_right = TEST_WRAP_CFG.clone();
            no_align_right.use_wrap_right_permille = 1; // 0.1%
            let cfg_no_align_right = mk_wrap_cfg(&no_align_right);

            let lines = wrap_test(&cfg_no_align_right, line, 6);
            assert_eq!(
                lines,
                vec![
                    vec![(*S1, "_012"), (*S2, "34"), (*SD, W)],
                    vec![(*SD, "_"), (*S2, "56")]
                ]
            );
        }
    }

    #[test]
    fn test_wrap_line_newlines<'a>() {
        fn mk_input(len: usize) -> LineSegments<'static, Style> {
            const IN: &str = "_0123456789abcdefZ";
            let v = &[*S1, *S2];
            let s1s2 = v.iter().cycle();
            let text: Vec<_> = IN.matches(|_| true).take(len + 1).collect();
            s1s2.zip(text.iter())
                .map(|(style, text)| (style.clone(), *text))
                .collect()
        }
        fn mk_input_nl(len: usize) -> LineSegments<'static, Style> {
            const NL: &str = "\n";
            let mut line = mk_input(len);
            line.push((*S2, NL));
            line
        }
        fn mk_expected<'a>(
            prepend: Option<(Style, &'a str)>,
            vec: &LineSegments<'a, Style>,
            from: usize,
            to: usize,
            append: Option<(Style, &'a str)>,
        ) -> LineSegments<'a, Style> {
            let mut result: Vec<_> = vec[from..to].iter().cloned().collect();
            if let Some(val) = append {
                result.push(val);
            }
            if let Some(val) = prepend {
                result.insert(0, val);
            }
            result
        }

        let cfg = mk_wrap_cfg(&TEST_WRAP_CFG);

        {
            let line = vec![(*S1, "_012"), (*S2, "345\n")];
            let lines = wrap_test(&cfg, line, 6);
            assert_eq!(lines, vec![vec![(*S1, "_012"), (*S2, "345\n")]]);
        }

        {
            for i in 0..=6 {
                let line = mk_input(i);
                let lines = wrap_test(&cfg, line, 6);
                assert_eq!(lines, vec![mk_input(i)]);

                let line = mk_input_nl(i);
                let lines = wrap_test(&cfg, line, 6);
                assert_eq!(lines, vec![mk_input_nl(i)]);
            }
        }

        {
            let line = mk_input_nl(9);
            let lines = wrap_test(&cfg, line, 3);
            let expected = mk_input_nl(9);
            let line1 = mk_expected(None, &expected, 0, 3, Some((*SD, &W)));
            let line2 = mk_expected(Some((*SD, "_")), &expected, 3, 5, Some((*SD, &W)));
            let line3 = mk_expected(Some((*SD, "_")), &expected, 5, 7, Some((*SD, &W)));
            let line4 = mk_expected(Some((*SD, "_")), &expected, 7, 11, None);
            assert_eq!(lines, vec![line1, line2, line3, line4]);
        }

        {
            let line = mk_input_nl(10);
            let lines = wrap_test(&cfg, line, 3);
            let expected = mk_input_nl(10);
            let line1 = mk_expected(None, &expected, 0, 3, Some((*SD, &W)));
            let line2 = mk_expected(Some((*SD, "_")), &expected, 3, 5, Some((*SD, &W)));
            let line3 = mk_expected(Some((*SD, "_")), &expected, 5, 7, Some((*SD, &W)));
            let line4 = mk_expected(Some((*SD, "_")), &expected, 7, 9, Some((*SD, &W)));
            let line5 = mk_expected(Some((*SD, "_")), &expected, 9, 11, Some((*S2, "\n")));
            assert_eq!(lines, vec![line1, line2, line3, line4, line5]);
        }

        {
            let line = vec![(*S1, "_abc"), (*S2, "01230123012301230123"), (*S1, "ZZZZZ")];

            let wcfg1 = mk_wrap_cfg(&WrapConfig {
                max_lines: 1,
                ..TEST_WRAP_CFG.clone()
            });
            let wcfg2 = mk_wrap_cfg(&WrapConfig {
                max_lines: 2,
                ..TEST_WRAP_CFG.clone()
            });
            let wcfg3 = mk_wrap_cfg(&WrapConfig {
                max_lines: 3,
                ..TEST_WRAP_CFG.clone()
            });

            let lines = wrap_line(&wcfg1, line.clone(), 4, &Style::default(), &None);
            assert_eq!(lines.len(), 1);
            assert_eq!(lines.last().unwrap().last().unwrap().1, "ZZZZZ");
            let lines = wrap_line(&wcfg2, line.clone(), 4, &Style::default(), &None);
            assert_eq!(lines.len(), 2);
            assert_eq!(lines.last().unwrap().last().unwrap().1, "ZZZZZ");
            let lines = wrap_line(&wcfg3, line.clone(), 4, &Style::default(), &None);
            assert_eq!(lines.len(), 3);
            assert_eq!(lines.last().unwrap().last().unwrap().1, "ZZZZZ");
        }
    }

    #[test]
    fn test_wrap_line_unicode() {
        let cfg = mk_wrap_cfg(&TEST_WRAP_CFG);

        // from UnicodeSegmentation documentation and the linked
        // Unicode Standard Annex #29
        let line = vec![(*S1, "_abc"), (*S2, "mnö̲"), (*S1, "xyz")];
        let lines = wrap_test(&cfg, line, 4);
        assert_eq!(
            lines,
            vec![
                vec![(*S1, "_abc"), (*SD, &W)],
                vec![(*SD, "_"), (*S2, "mnö̲"), (*SD, &W)],
                vec![(*SD, "_"), (*S1, "xyz")]
            ]
        );

        // Not working: Tailored grapheme clusters: क्षि  = क् + षि
        let line = vec![(*S1, "_abc"), (*S2, "deநி"), (*S1, "ghij")];
        let lines = wrap_test(&cfg, line, 4);
        assert_eq!(
            lines,
            vec![
                vec![(*S1, "_abc"), (*SD, &W)],
                vec![(*SD, "_"), (*S2, "deநி"), (*SD, &W)],
                vec![(*SD, "_"), (*S1, "ghij")]
            ]
        );
    }

    const HUNK_ZERO_DIFF: &str = "\
diff --git i/a.py w/a.py
index 223ca50..e69de29 100644
--- i/a.py
+++ w/a.py
@@ -4,3 +15,3 @@
 abcdefghijklmnopqrstuvwxzy 0123456789 0123456789 0123456789 0123456789 0123456789
-a = 1
+a = 2
";

    const HUNK_ZERO_LARGE_LINENUMBERS_DIFF: &str = "\
diff --git i/a.py w/a.py
index 223ca50..e69de29 100644
--- i/a.py
+++ w/a.py
@@ -10,3 +101999,3 @@
 abcdefghijklmnopqrstuvwxzy 0123456789 0123456789 0123456789 0123456789 0123456789
-a = 1
+a = 2
";

    const HUNK_MP_DIFF: &str = "\
diff --git i/a.py w/a.py
index 223ca50..e69de29 100644
--- i/a.py
+++ w/a.py
@@ -4,3 +15,3 @@
 abcdefghijklmnopqrstuvwxzy 0123456789 0123456789 0123456789 0123456789 0123456789
-a = 0123456789 0123456789 0123456789 0123456789 0123456789
+b = 0123456789 0123456789 0123456789 0123456789 0123456789
";

    const HUNK_ALIGN_DIFF_HEADER: &str = "--- a\n+++ b\n@@ -1,1 +1,1 @@\n";
    const HUNK_ALIGN_DIFF_SHORT: &str = ".........1.........2....\n";
    const HUNK_ALIGN_DIFF_LONG: &str =
        ".........1.........2.........3.........4.........5.........6\n";

    #[test]
    fn test_wrap_with_unequal_hunk_zero_width() {
        let mut config = make_config_from_args(&default_wrap_cfg_plus(&[
            "--side-by-side",
            "--line-numbers-left-format",
            "│L│",
            "--line-numbers-right-format",
            "│RRRR│",
            "--width",
            "40",
            "--line-fill-method",
            "spaces",
        ]));
        config.truncation_symbol = ">".into();

        let output = run_delta(HUNK_ZERO_DIFF, &config);
        let output = strip_ansi_codes(&output);
        let lines: Vec<_> = output.lines().skip(crate::config::HEADER_LEN).collect();
        let expected = vec![
            "│L│abcdefghijklm+   │RRRR│abcdefghijklm+",
            "│L│nopqrstuvwxzy+   │RRRR│nopqrstuvwxzy+",
            "│L│ 0123456789 0+   │RRRR│ 0123456789 0+",
            "│L│123456789 012+   │RRRR│123456789 012+",
            "│L│3456789 01234567>│RRRR│3456789 01234>",
            "│L│a = 1            │RRRR│a = 2         ",
        ];
        assert_eq!(lines, expected);
    }

    #[test]
    fn test_wrap_with_large_hunk_zero_line_numbers() {
        let mut config = make_config_from_args(&default_wrap_cfg_plus(&[
            "--side-by-side",
            "--line-numbers-left-format",
            "│LLL│",
            "--line-numbers-right-format",
            "│WW {nm} +- {np:2} WW│",
            "--width",
            "60",
            "--line-fill-method",
            "ansi",
        ]));
        config.truncation_symbol = ">".into();

        let output = run_delta(HUNK_ZERO_LARGE_LINENUMBERS_DIFF, &config);
        let output = strip_ansi_codes(&output);
        let lines: Vec<_> = output.lines().skip(crate::config::HEADER_LEN).collect();
        let expected = vec![
            "│LLL│abcde+                   │WW   10   +- 101999 WW│abcde+",
            "│LLL│fghij+                   │WW        +-        WW│fghij+",
            "│LLL│klmno+                   │WW        +-        WW│klmno+",
            "│LLL│pqrst+                   │WW        +-        WW│pqrst+",
            "│LLL│uvwxzy 0123456789 012345>│WW        +-        WW│uvwxz>",
            "│LLL│a = 1                    │WW        +- 102000 WW│a = 2",
        ];
        assert_eq!(lines, expected);
    }

    #[test]
    fn test_wrap_with_keep_markers() {
        use crate::features::side_by_side::ansifill::ODD_PAD_CHAR;
        let mut config = make_config_from_args(&default_wrap_cfg_plus(&[
            "--side-by-side",
            "--keep-plus-minus-markers",
            "--width",
            "45",
        ]));
        config.truncation_symbol = ">".into();

        let output = run_delta(HUNK_MP_DIFF, &config);
        let output = strip_ansi_codes(&output);
        let lines: Vec<_> = output.lines().skip(crate::config::HEADER_LEN).collect();
        let expected = vec![
            "│ 4  │ abcdefghijklmn+ │ 15 │ abcdefghijklmn+",
            "│    │ opqrstuvwxzy 0+ │    │ opqrstuvwxzy 0+",
            "│    │ 123456789 0123+ │    │ 123456789 0123+",
            "│    │ 456789 0123456+ │    │ 456789 0123456+",
            "│    │ 789 0123456789> │    │ 789 0123456789>",
            "│ 5  │-a = 0123456789+ │ 16 │+b = 0123456789+",
            "│    │  0123456789 01+ │    │  0123456789 01+",
            "│    │ 23456789 01234+ │    │ 23456789 01234+",
            "│    │ 56789 01234567+ │    │ 56789 01234567+",
            "│    │ 89              │    │ 89",
            // this is place where ^ ODD_PAD_CHAR is inserted due to the odd 45 width
        ];
        assert_eq!(lines, expected);

        for line in lines {
            assert_eq!(line.chars().nth(22), Some(ODD_PAD_CHAR));
        }
    }

    #[test]
    fn test_alignment_2_lines_vs_3_lines() {
        let config =
            make_config_from_args(&default_wrap_cfg_plus(&["--side-by-side", "--width", "55"]));
        {
            let output = run_delta(
                &format!(
                    "{}-{}+{}",
                    HUNK_ALIGN_DIFF_HEADER, HUNK_ALIGN_DIFF_SHORT, HUNK_ALIGN_DIFF_LONG
                ),
                &config,
            );
            let output = strip_ansi_codes(&output);
            let lines: Vec<_> = output.lines().skip(crate::config::HEADER_LEN).collect();
            let expected = vec![
                "│ 1  │.........1.........2< │ 1  │.........1.........2+",
                "│    │                >.... │    │.........3.........4+",
                "│    │                      │    │.........5.........6",
                // place where ODD_PAD_CHAR ^ is inserted due to the odd 55 width
            ];
            assert_eq!(lines, expected);
        }

        {
            let output = run_delta(
                &format!(
                    "{}-{}+{}",
                    HUNK_ALIGN_DIFF_HEADER, HUNK_ALIGN_DIFF_LONG, HUNK_ALIGN_DIFF_SHORT
                ),
                &config,
            );
            let output = strip_ansi_codes(&output);
            let lines: Vec<_> = output.lines().skip(crate::config::HEADER_LEN).collect();
            let expected = vec![
                "│ 1  │.........1.........2+ │ 1  │.........1.........2<",
                "│    │.........3.........4+ │    │                >....",
                "│    │.........5.........6  │    │",
            ];
            assert_eq!(lines, expected);
        }
    }

    #[test]
    fn test_alignment_1_line_vs_3_lines() {
        let config = make_config_from_args(&default_wrap_cfg_plus(&[
            "--side-by-side",
            "--width",
            "61",
            "--line-fill-method",
            "spaces",
        ]));

        {
            let output = run_delta(
                &format!(
                    "{}-{}+{}",
                    HUNK_ALIGN_DIFF_HEADER, HUNK_ALIGN_DIFF_SHORT, HUNK_ALIGN_DIFF_LONG
                ),
                &config,
            );
            let output = strip_ansi_codes(&output);
            let lines: Vec<_> = output.lines().skip(crate::config::HEADER_LEN).collect();
            let expected = vec![
                "│ 1  │.........1.........2....│ 1  │.........1.........2...+",
                "│    │                        │    │......3.........4......+",
                "│    │                        │    │...5.........6          ",
            ];
            assert_eq!(lines, expected);
        }

        {
            let output = run_delta(
                &format!(
                    "{}-{}+{}",
                    HUNK_ALIGN_DIFF_HEADER, HUNK_ALIGN_DIFF_LONG, HUNK_ALIGN_DIFF_SHORT
                ),
                &config,
            );
            let output = strip_ansi_codes(&output);
            let lines: Vec<_> = output.lines().skip(crate::config::HEADER_LEN).collect();
            let expected = vec![
                "│ 1  │.........1.........2...+│ 1  │.........1.........2....",
                "│    │......3.........4......+│    │",
                "│    │...5.........6          │    │",
            ];
            assert_eq!(lines, expected);
        }
    }

    #[test]
    fn test_wrap_max_lines_2() {
        // TODO overriding is not possible, need to change config directly
        let mut config = make_config_from_args(&default_wrap_cfg_plus(&[
            // "--wrap-max-lines",
            // "2",
            "--side-by-side",
            "--width",
            "72",
            "--line-fill-method",
            "spaces",
        ]));
        config.truncation_symbol = ">".into();

        {
            let output = run_delta(
                &format!(
                    "{}-{}+{}",
                    HUNK_ALIGN_DIFF_HEADER, HUNK_ALIGN_DIFF_SHORT, HUNK_ALIGN_DIFF_LONG
                ),
                &config,
            );
            let output = strip_ansi_codes(&output);
            let lines: Vec<_> = output.lines().skip(crate::config::HEADER_LEN).collect();
            let expected = vec![
                "│ 1  │.........1.........2....      │ 1  │.........1.........2.........+",
                "│    │                              │    │3.........4.........5........+",
                "│    │                              │    │.6                            ",
            ];
            assert_eq!(lines, expected);
        }

        {
            config.wrap_config.max_lines = 2;
            let output = run_delta(
                &format!(
                    "{}-{}+{}",
                    HUNK_ALIGN_DIFF_HEADER, HUNK_ALIGN_DIFF_SHORT, HUNK_ALIGN_DIFF_LONG
                ),
                &config,
            );
            let output = strip_ansi_codes(&output);
            let lines: Vec<_> = output.lines().skip(crate::config::HEADER_LEN).collect();
            let expected = vec![
                "│ 1  │.........1.........2....      │ 1  │.........1.........2.........+",
                "│    │                              │    │3.........4.........5........>",
            ];
            assert_eq!(lines, expected);
        }
    }
}
