mod actions;
mod config;
mod daemon;
mod input;
mod layout;
mod media;
mod render;
mod sliders;
mod state;
mod transport;

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use clap::{Parser, Subcommand};

use actions::Keyboard;
use config::Config;
use layout::{Item, Slider};
use media::NowPlaying;
use render::{keys, Canvas};

const PANEL: (u32, u32) = (2170, 60);

#[derive(Parser)]
#[command(name = "penguinbar", about = "App-aware Touch Bar daemon for Linux")]
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
    /// Run the daemon
    Run,
    /// Print the Touch Bar's DRM card, connector and mode
    Info,
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
        /// Show an open slider instead of the app's keys
        #[arg(long)]
        slider: bool,
    },
    /// Print touches on the bar as they happen
    Touch,
    /// Print what the music and video players are doing
    Media,
    /// Press a key on the virtual keyboard, like KEY_VOLUMEUP or KEY_LEFTCTRL+KEY_C
    Key { name: String },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = if cli.verbose {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };
    tracing_subscriber::fmt().with_max_level(level).init();

    match cli.command {
        Command::Run => daemon::run(),
        Command::Info => transport::drm::info(),
        Command::Row {
            class,
            expanded,
            playing,
            slider,
        } => {
            let open = if slider {
                Some(Item::Slider {
                    kind: Slider::Brightness,
                    value: 0.6,
                    left: 1200.0,
                })
            } else {
                None
            };
            let layout = Config::load()?.layout_for(&class, expanded, playing, open);
            let now = if playing {
                Some(NowPlaying {
                    app: class.clone(),
                    title: String::new(),
                    playing: true,
                    position: Duration::from_secs(64),
                    length: Duration::from_secs(210),
                })
            } else {
                None
            };
            show(cli.png, |canvas| {
                keys::draw_row(canvas, &layout, None, now.as_ref())
            })
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
        Command::Media => {
            let (tx, rx) = mpsc::channel();
            media::spawn_watcher(tx);
            println!("watching the players. Ctrl-C to exit.");
            for event in rx {
                println!("{event:?}");
            }
            Ok(())
        }
        Command::Key { name } => Keyboard::new()?.tap(&actions::parse_keys(&name)?),
    }
}

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
