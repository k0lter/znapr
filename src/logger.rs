use std::io::IsTerminal;

use gilt::color::ColorSystem;
use gilt::console::Console;
use tracing::level_filters::LevelFilter;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::{self, FmtContext};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

/// Semantic markup tags and their gilt inline style equivalents.
/// Gilt does not resolve custom theme names in markup yet, so we expand them here.
const SEMANTIC_TAGS: &[(&str, &str)] = &[
    ("cfg", "#22d3ee"),
    ("data", "#3b82f6"),
    ("err", "#d946ef"),
    ("result", "bold #22c55e"),
    ("sep", "#94a3b8"),
    ("host", "bold #14b8a6"),
    ("cmd", "bold #f59e0b"),
    ("path", "underline #bcbcf5"),
    ("ok", "bold #10b981"),
];

fn console() -> Console {
    Console::builder()
        .force_terminal(true)
        .no_color(false)
        .color_system_override(ColorSystem::TrueColor)
        .markup(true)
        // ReprHighlighter paints a matched number bold cyan and stops before a
        // unit letter, so `1.24T` becomes cyan `1.` plus the cell color on `24T`.
        .highlight(false)
        .width(200)
        .build()
}

fn with_console<F, R>(f: F) -> R
where
    F: FnOnce(&Console) -> R,
{
    let console = console();
    f(&console)
}

fn expand_semantic_tags(input: &str) -> String {
    let mut result = input.to_string();
    for (name, style) in SEMANTIC_TAGS {
        result = result.replace(&format!("[{name}]"), &format!("[{style}]"));
    }
    result
}

pub fn render_markup(input: &str) -> String {
    let expanded = expand_semantic_tags(input);
    with_console(|console| {
        let text = console.render_str(&expanded, None, None, None);
        let segments = console.render(&text, None);
        console.render_buffer(&segments).trim_end().to_string()
    })
}

/// Render markup to plain text: tags are interpreted (and thus removed) but no
/// ANSI colour is emitted. Used to sanitise messages that go into e-mail.
pub fn render_plain(input: &str) -> String {
    let expanded = expand_semantic_tags(input);
    let console = Console::builder()
        .force_terminal(false)
        .no_color(true)
        .markup(true)
        .build();
    let text = console.render_str(&expanded, None, None, None);
    let segments = console.render(&text, None);
    console.render_buffer(&segments).trim_end().to_string()
}

/// Markup for stdout. A pipe or a file gets plain text so ANSI codes are not
/// counted as columns by `less` or an editor. Logs stay on [`render_markup`]:
/// they go to stderr, which is still a terminal when only stdout is redirected.
pub fn render_stdout(input: &str) -> String {
    if std::io::stdout().is_terminal() {
        render_markup(input)
    } else {
        render_plain(input)
    }
}

struct GiltFormatter;

impl<S, N> FormatEvent<S, N> for GiltFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let level = *event.metadata().level();

        let level_tag = match level {
            Level::ERROR => render_markup("[bold #e5484d]error[/]"),
            Level::WARN => render_markup("[bold #f76b15]warn[/]"),
            Level::INFO => render_markup("[bold #2f81f7]info[/]"),
            Level::DEBUG => render_markup("[bold #8b5cf6]debug[/]"),
            Level::TRACE => render_markup("[dim #64748b]trace[/]"),
        };

        let mut buf = String::new();
        ctx.field_format()
            .format_fields(Writer::new(&mut buf), event)?;

        let styled_msg = render_markup(&buf);

        writeln!(writer, "{level_tag} {styled_msg}")
    }
}

pub fn init(verbosity: u8, quiet: bool) {
    let filter = if quiet {
        LevelFilter::ERROR
    } else {
        match verbosity {
            0 => LevelFilter::WARN,
            1 => LevelFilter::INFO,
            2 => LevelFilter::DEBUG,
            _ => LevelFilter::TRACE,
        }
    };

    let znapr_directive = format!(
        "znapr={}",
        match filter {
            LevelFilter::ERROR => "error",
            LevelFilter::WARN => "warn",
            LevelFilter::INFO => "info",
            LevelFilter::DEBUG => "debug",
            LevelFilter::TRACE => "trace",
            LevelFilter::OFF => "off",
        }
    );

    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::OFF.into())
        .from_env_lossy()
        .add_directive(znapr_directive.parse().expect("valid filter directive"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().event_format(GiltFormatter))
        .init();
}

/// Whether the very-verbose (`-vv`, DEBUG) level is active.
/// Drives rsync progress display and output capture, like cleo's `is_very_verbose`.
pub fn enabled_debug() -> bool {
    tracing::enabled!(tracing::Level::DEBUG)
}

/// Whether a live progress bar should be shown: only on an interactive stdout
/// and only at the default (WARN) level, so it never fights `-v` log output.
pub fn progress_enabled() -> bool {
    std::io::stdout().is_terminal()
        && tracing::enabled!(tracing::Level::WARN)
        && !tracing::enabled!(tracing::Level::INFO)
}

#[cfg(test)]
mod tests {
    use super::render_markup;

    #[test]
    fn size_markup_keeps_one_color() {
        for sample in ["[data]1.24T[/]", "[data]10.4T[/]", "[data]445K[/]"] {
            let rendered = render_markup(sample);
            assert_eq!(
                rendered.matches("\u{1b}[0m").count(),
                1,
                "{sample} => {rendered:?}"
            );
            assert!(
                !rendered.contains("\u{1b}[1;36m"),
                "{sample} => {rendered:?}"
            );
        }
    }
}

/// Alias for `bail!` — messages may contain semantic markup tags, rendered at display time.
#[macro_export]
macro_rules! error_msg {
    ($msg:expr) => {
        anyhow::bail!($msg)
    };
    ($fmt:expr, $($arg:tt)*) => {
        anyhow::bail!($fmt, $($arg)*)
    };
}
