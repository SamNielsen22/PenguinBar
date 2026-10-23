mod actions;
mod config;
mod daemon;
mod input;
mod layout;
mod media;
mod render;
mod state;
mod transport;

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use evdev::KeyCode;

use actions::Keyboard;
use config::Config;
use media::NowPlaying;
use render::{keys, Canvas, Rgb};
use transport::Display;

// Used for --png, where there is no hardware to ask.
const PANEL: (u32, u32) = (2170, 60);
const BG: Rgb = (0.0, 0.0, 0.0);
const FG: Rgb = (1.0, 1.0, 1.0);

#[derive(Parser)]
#[command(name = "magicbar", about = "App-aware Touch Bar daemon for Linux")]
struct Cli {
    /// Print debug output
    #[arg(short, long)]
    verbose: bool,
    /// Render to a PNG file instead of the bar
    #[arg(long, value_name = "FILE")]
    png: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Info,
    Fill { colour: String },
    Text { text: String },
    /// Draw the bar as an app would see it
    Row {
        /// Window class, like firefox. Leave empty for a bar with no app keys
        #[arg(default_value = "")]
        class: String,
        /// Show the strip expanded over the app's keys
        #[arg(long)]
        expanded: bool,
        /// Pretend a track is playing, to see the media widget
        #[arg(long)]
        playing: bool,
    },
    Touch,
    /// Print what the music or video player is doing
    Media {
        /// Toggle play and pause first
        #[arg(long)]
        play_pause: bool,
        /// Jump to this many seconds into the track first
        #[arg(long)]
        seek: Option<u64>,
    },
    Key { name: String },
    Run,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = if cli.verbose { tracing::Level::DEBUG } else { tracing::Level::INFO };
    tracing_subscriber::fmt().with_max_level(level).init();

    match cli.command {
        Command::Info => transport::drm::info(),
        Command::Fill { colour } => {
            let colour = parse_colour(&colour)?;
            show(cli.png, |canvas| canvas.fill(colour))
        }
        Command::Text { text } => show(cli.png, |canvas| {
            canvas.fill(BG)?;
            canvas.text(&text, 28.0, FG)
        }),
        Command::Row { class, expanded, playing } => {
            let layout = Config::load()?.layout_for(&class, expanded, playing);
            let now = if playing {
                Some(NowPlaying {
                    app: class.clone(),
                    playing: true,
                    position: Duration::from_secs(64),
                    length: Duration::from_secs(210),
                })
            } else {
                None
            };
            show(cli.png, |canvas| keys::draw_row(canvas, &layout, None, now.as_ref()))
        }
        Command::Touch => {
            let (tx, rx) = mpsc::channel();
            input::spawn_reader(input::find_touch_device()?, tx)?;
            println!("touch the bar. Ctrl-C to exit.");
            for event in rx {
                println!("{event:?}");
            }
            Ok(())
        }
        Command::Media { play_pause, seek } => {
            if play_pause {
                media::play_pause()?;
            }
            if let Some(seconds) = seek {
                media::seek_to(Duration::from_secs(seconds))?;
            }
            let (tx, rx) = mpsc::channel();
            media::spawn_watcher(tx);
            println!("watching the player. Ctrl-C to exit.");
            for event in rx {
                println!("{event:?}");
            }
            Ok(())
        }
        Command::Key { name } => {
            let key: KeyCode = name
                .parse()
                .map_err(|_| anyhow::anyhow!("unknown key {name}. Names look like KEY_ESC"))?;
            Keyboard::new()?.tap(key)
        }
        Command::Run => daemon::run(),
    }
}

// Draw, then save a PNG or show it on the bar until Enter.
fn show<F>(png: Option<PathBuf>, draw: F) -> anyhow::Result<()>
where
    F: FnOnce(&mut Canvas) -> anyhow::Result<()>,
{
    if let Some(path) = png {
        let (width, height) = PANEL;
        let mut canvas = Canvas::new(width, height)?;
        draw(&mut canvas)?;
        canvas.save_png(&path)?;
        println!("wrote {}", path.display());
    } else {
        let mut display = transport::drm::DrmDisplay::open()?;
        let (width, height) = display.size();
        let mut canvas = Canvas::new(width, height)?;
        draw(&mut canvas)?;
        display.present(&canvas)?;

        println!("showing on the bar. Press Enter to exit.");
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
    }
    Ok(())
}

fn parse_colour(s: &str) -> anyhow::Result<Rgb> {
    let hex = s.trim_start_matches('#');
    let n = match u32::from_str_radix(hex, 16) {
        Ok(n) if hex.len() == 6 => n,
        _ => anyhow::bail!("colour must look like #rrggbb, got {s}"),
    };
    let channel = |shift: u32| ((n >> shift) & 0xff) as f64 / 255.0;
    Ok((channel(16), channel(8), channel(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_colours() {
        assert_eq!(parse_colour("#ff0000").unwrap(), (1.0, 0.0, 0.0));
        assert_eq!(parse_colour("00ff00").unwrap(), (0.0, 1.0, 0.0));
    }

    #[test]
    fn rejects_bad_colours() {
        assert!(parse_colour("#ff00").is_err());
        assert!(parse_colour("zz0000").is_err());
    }
}
