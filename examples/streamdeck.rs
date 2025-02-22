
#[macro_use] extern crate log;
extern crate simplelog;
use simplelog::{TermLogger, LevelFilter, TerminalMode, ColorChoice};

extern crate structopt;
use structopt::StructOpt;

extern crate humantime;
use humantime::Duration;

use streamdeck::{StreamDeck, Filter, Colour, ImageOptions, Error};

#[derive(StructOpt)]
#[structopt(name = "streamdeck-cli", about = "A CLI for the Elgato StreamDeck")]
struct Options {

    #[structopt(subcommand)]
    cmd: Commands,

    #[structopt(flatten)]
    filter: Filter,

    #[structopt(long = "log-level", default_value = "info")]
    /// Enable verbose logging
    level: LevelFilter,
}

#[derive(StructOpt)]
pub enum Commands {
    /// Reset the attached device
    Reset,
    /// Fetch the device firmware version
    Version,
    /// Set device display brightness
    SetBrightness{
        /// Brightness value from 0 to 100
        brightness: u8,
    },
    /// Fetch button states
    GetButtons {
        #[structopt(long)]
        /// Timeout for button reading
        timeout: Option<Duration>,

        #[structopt(long)]
        /// Read continuously
        continuous: bool,
    },
    /// Fetch button states
    GetInput {
        #[structopt(long)]
        /// Timeout for input reading
        timeout: Option<Duration>,

        #[structopt(long)]
        /// Read continuously
        continuous: bool,
    },    
    /// Set button colours
    SetColour {
        /// Index of button to be set
        key: u8,

        #[structopt(flatten)]
        colour: Colour,
    },
    /// Set button images
    SetImage {
        /// Index of button to be set
        key: u8,

        /// Image file to be loaded
        file: String,

        #[structopt(flatten)]
        opts: ImageOptions,
    },
    SetLcdImage {
        x: u16,
        y: u16,
        file: String,
    },
}

fn main() {
    // Parse options
    let opts = Options::from_args();

    // Setup logging
    let mut config = simplelog::ConfigBuilder::new();
    config.set_time_level(LevelFilter::Off);

    TermLogger::init(opts.level, config.build(), TerminalMode::Mixed, ColorChoice::Auto).unwrap();

    // Connect to device
    let mut deck = match StreamDeck::connect(opts.filter.vid, opts.filter.pid, opts.filter.serial) {
        Ok(d) => d,
        Err(e) => {
            error!("Error connecting to streamdeck: {:?}", e);
            return
        }
    };

    let serial = deck.serial().unwrap();
    info!("Connected to device (vid: {:04x} pid: {:04x} serial: {})", 
            opts.filter.vid, opts.filter.pid, serial);

    // Run the command
    if let Err(e) = do_command(&mut deck, opts.cmd) {
        error!("Command error: {:?}", e);
    }
}

fn do_command(deck: &mut StreamDeck, cmd: Commands) -> Result<(), Error> {
    match cmd {
        Commands::Reset => {
            deck.reset()?;
        },
        Commands::Version => {
            let version = deck.version()?;
            info!("Firmware version: {}", version);
        }
        Commands::SetBrightness{brightness} => {
            deck.set_brightness(brightness)?;
        },
        Commands::GetButtons{timeout, continuous} => {
            loop {
                let buttons = deck.read_buttons(timeout.map(|t| *t ))?;
                info!("buttons: {:?}", buttons);

                if !continuous {
                    break
                }
            }
        },
        Commands::GetInput{timeout, continuous} => {
            loop {
                let input = deck.read_input(timeout.map(|t| *t ))?;
                info!("input: {:?}", input);

                if !continuous {
                    break
                }
            }
        },        
        Commands::SetColour{key, colour} => {
            info!("Setting key {} colour to: ({:?})", key, colour);
            deck.set_button_rgb(key, &colour)?;
        },
        Commands::SetImage{key, file, opts} => {
            info!("Setting key {} to image: {}", key, file);
            deck.set_button_file(key, &file, &opts)?;
        }
        Commands::SetLcdImage{x, y, file} => {
            info!("writing {} to {},{}", file, x, y);

            let (w, h) = (800, 100);
            let mut buf = vec![0u8; w * h * 3];
            let c = [255, 255, 255];
            for i in 0..w {
                let x = i;
                let y = i % h;
                let n = (x + (y * w)) * 3;
                // println!("{} {} {}", x, y, n);
                buf[n + 0] = c[0];
                buf[n + 1] = c[1];
                buf[n + 2] = c[2];
            }
            deck.write_lcd_raw(x, y, w as u16, h as u16, &buf)?;


            for key in 0..8 {
                let (w, h) = (120, 120);
                let mut buf = vec![0u8; w * h * 3];
                let c = [255, 255, 255];
                for i in 0..h {
                    let x = i;
                    let y = i;
                    let n = (x + (y * w)) * 3;
                    buf[n + 0] = c[0];
                    buf[n + 1] = c[1];
                    buf[n + 2] = c[2];
                }
                deck.write_button_raw(key, w as u16, h as u16, &buf)?;
            }

            // deck.set_button_file(key, &file, &opts)?;
        }        
    }

    Ok(())
}
