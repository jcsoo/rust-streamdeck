use std::{io::Error as IoError};
use std::time::Duration;

#[macro_use]
extern crate log;

extern crate hidapi;
use hidapi::{HidApi, HidDevice, HidError};

extern crate image;
use image::{DynamicImage, ImageBuffer, ImageError, Rgb};

pub mod images;
use crate::images::{apply_transform, encode_jpeg};
pub use crate::images::{Colour, ImageOptions};

pub mod info;
pub use info::*;

use imageproc::drawing::draw_text_mut;
use rusttype::{Font, Scale};
use std::str::FromStr;
use thiserror::Error;

/// StreamDeck object
pub struct StreamDeck {
    kind: Kind,
    device: HidDevice,
}

/// Helper object for filtering device connections
#[cfg(feature = "structopt")]
#[derive(structopt::StructOpt)]
pub struct Filter {
    #[structopt(long, default_value="0fd9", parse(try_from_str=u16_parse_hex), env="USB_VID")]
    /// USB Device Vendor ID (VID) in hex
    pub vid: u16,

    #[structopt(long, default_value="0063", parse(try_from_str=u16_parse_hex), env="USB_PID")]
    /// USB Device Product ID (PID) in hex
    pub pid: u16,

    #[structopt(long, env = "USB_SERIAL")]
    /// USB Device Serial
    pub serial: Option<String>,
}

fn u16_parse_hex(s: &str) -> Result<u16, std::num::ParseIntError> {
    u16::from_str_radix(s, 16)
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Hid(#[from] HidError),
    #[error(transparent)]
    Io(#[from] IoError),
    #[error(transparent)]
    Image(#[from] ImageError),

    #[error("invalid image size")]
    InvalidImageSize,
    #[error("invalid key index")]
    InvalidKeyIndex,
    #[error("unrecognised pid")]
    UnrecognisedPID,
    #[error("no data")]
    NoData,
}

pub struct DeviceImage {
    data: Vec<u8>,
}

impl DeviceImage {
    /// Constructs [DeviceImage] from a byte array
    pub fn from_bytes(data: Vec<u8>) -> Self {
        Self::from(data)
    }
}

impl From<Vec<u8>> for DeviceImage {
    fn from(data: Vec<u8>) -> Self {
        Self {
            data
        }
    }
}

/// Device USB Product Identifiers (PIDs)
pub mod pids {
    pub const ORIGINAL: u16 = 0x0060;
    pub const ORIGINAL_V2: u16 = 0x006d;
    pub const MINI: u16 = 0x0063;
    pub const XL: u16 = 0x006c;
    pub const MK2: u16 = 0x0080;
    pub const PLUS: u16 = 0x0084;
}

impl StreamDeck {
    /// Connect to a streamdeck device
    pub fn connect(vid: u16, pid: u16, serial: Option<String>) -> Result<StreamDeck, Error> {
        // Create new API
        let api = HidApi::new()?;
        StreamDeck::connect_with_hid(&api, vid, pid, serial)
    }

    /// Connect to a streamdeck device with an already initialise HidApi instance
    pub fn connect_with_hid(
        api: &HidApi,
        vid: u16,
        pid: u16,
        serial: Option<String>,
    ) -> Result<StreamDeck, Error> {
        // Match info based on PID
        let kind = match pid {
            pids::ORIGINAL => Kind::Original,
            pids::MINI => Kind::Mini,

            pids::ORIGINAL_V2 => Kind::OriginalV2,
            pids::XL => Kind::Xl,
            pids::MK2 => Kind::Mk2,
            pids::PLUS => Kind::Plus,

            _ => return Err(Error::UnrecognisedPID),
        };

        debug!("Device info: {:?}", kind);

        // Attempt to connect to device
        let device = match &serial {
            Some(s) => api.open_serial(vid, pid, s),
            None => api.open(vid, pid),
        }?;

        // Return streamdeck object
        Ok(StreamDeck { device, kind })
    }

    /// Fetch the connected device kind
    ///
    /// This can be used to retrieve related device information such as
    /// images sizes and modes
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// Fetch the device manufacturer string
    pub fn manufacturer(&mut self) -> Result<String, Error> {
        let s = self.device.get_manufacturer_string()?;
        Ok(s.unwrap())
    }

    /// Fetch the device product string
    pub fn product(&mut self) -> Result<String, Error> {
        let s = self.device.get_product_string()?;
        Ok(s.unwrap())
    }

    /// Fetch the device serial
    pub fn serial(&mut self) -> Result<String, Error> {
        let s = self.device.get_serial_number_string()?;
        Ok(s.unwrap())
    }

    /// Fetch the device firmware version
    pub fn version(&mut self) -> Result<String, Error> {
        let mut buff = [0u8; 17];
        buff[0] = if self.kind.is_v2() { 0x05 } else { 0x04 };

        let _s = self.device.get_feature_report(&mut buff)?;

        let offset = if self.kind.is_v2() { 6 } else { 5 };
        Ok(std::str::from_utf8(&buff[offset..]).unwrap().to_string())
    }

    /// Reset the connected device
    pub fn reset(&mut self) -> Result<(), Error> {
        let mut cmd = [0u8; 17];

        if self.kind.is_v2() {
            cmd[..2].copy_from_slice(&[0x03, 0x02]);
        } else {
            cmd[..2].copy_from_slice(&[0x0b, 0x63]);
        }

        self.device.send_feature_report(&cmd)?;

        Ok(())
    }

    /// Set the device display brightness (in percent)
    pub fn set_brightness(&mut self, brightness: u8) -> Result<(), Error> {
        let mut cmd = [0u8; 17];

        let brightness = brightness.min(100);

        if self.kind.is_v2() {
            cmd[..3].copy_from_slice(&[0x03, 0x08, brightness]);
        } else {
            cmd[..6].copy_from_slice(&[0x05, 0x55, 0xaa, 0xd1, 0x01, brightness]);
        }

        self.device.send_feature_report(&cmd)?;

        Ok(())
    }

    /// Set blocking mode
    ///
    /// See: `read_buttons` for discussion of this functionality
    pub fn set_blocking(&mut self, blocking: bool) -> Result<(), Error> {
        self.device.set_blocking_mode(blocking)?;

        Ok(())
    }

    pub fn read_input(&mut self, timeout: Option<Duration>) -> Result<Input, Error> {
        let mut cmd = [0u8; 36];
        let keys = self.kind.keys() as usize;
        let offset = self.kind.key_data_offset();

        match timeout {
            Some(t) => self
                .device
                .read_timeout(&mut cmd[..keys + offset + 1], t.as_millis() as i32)?,
            None => self.device.read(&mut cmd[..keys + offset + 1])?,
        };

        if cmd[0] == 0 {
            return Err(Error::NoData);
        }

        match (cmd[1], cmd[2]) {
            (0x00, 0x08) => {
                let mut out = vec![0u8; keys];
                match self.kind.key_direction() {
                    KeyDirection::RightToLeft => {
                        for (i, val) in out.iter_mut().enumerate() {
                            // In right-to-left mode(original Streamdeck) the first key has index 1,
                            // so we don't add the +1 here.
                            *val = cmd[offset + self.translate_key_index(i as u8)? as usize];
                        }
                    }
                    KeyDirection::LeftToRight => {
                        out[0..keys].copy_from_slice(&cmd[1 + offset..1 + offset + keys]);
                    }
                }
                Ok(Input::Button(out))
            },
            (0x02, 0x0e) => {
                match (cmd[4], cmd[5]) {
                    (0x01, 0x01)  => {
                        let (x, y) = (u16::from_le_bytes([cmd[6], cmd[7]]), cmd[8] as u16);
                        Ok(Input::Touch(TouchInput::Short { x, y }))
                    },
                    (0x02, 0x01) => {
                        let (x, y) = (u16::from_le_bytes([cmd[6], cmd[7]]), cmd[8] as u16);
                        Ok(Input::Touch(TouchInput::Long { x, y }))
                    },
                    (0x03, 0x00) => {
                        let (x0, y0) = (u16::from_le_bytes([cmd[6], cmd[7]]), cmd[8] as u16);
                        let x1 = u16::from_le_bytes([cmd[10], cmd[11]]);
                        let y1 = y0;
                        Ok(Input::Touch(TouchInput::Swipe { x0, y0, x1, y1 }))
                    }
                    _ => unimplemented!(),
                }
            },
            (0x03, 0x05) => {
                match cmd[4] {
                    0 => {
                        Ok(Input::Knob(KnobInput::Press(cmd[5..9].to_vec())))
                    },
                    1 => {
                        Ok(Input::Knob(KnobInput::Rotate(
                            vec![
                                cmd[5] as i8,
                                cmd[6] as i8,
                                cmd[7] as i8,
                                cmd[8] as i8,
                            ]
                        )))
                    },
                    _ => unimplemented!()
                }
            },
            _ => Ok(Input::Other),
        }

    }

    /// Fetch button states
    ///
    /// In blocking mode this will wait until a report packet has been received
    /// (or the specified timeout has elapsed). In non-blocking mode this will return
    /// immediately with a zero vector if no data is available
    pub fn read_buttons(&mut self, timeout: Option<Duration>) -> Result<Vec<u8>, Error> {
        let mut cmd = [0u8; 36];
        let keys = self.kind.keys() as usize;
        let offset = self.kind.key_data_offset();

        match timeout {
            Some(t) => self
                .device
                .read_timeout(&mut cmd[..keys + offset + 1], t.as_millis() as i32)?,
            None => self.device.read(&mut cmd[..keys + offset + 1])?,
        };

        if cmd[0] == 0 {
            return Err(Error::NoData);
        }

        println!("{:02x?}", cmd);

        let mut out = vec![0u8; keys];
        match self.kind.key_direction() {
            KeyDirection::RightToLeft => {
                for (i, val) in out.iter_mut().enumerate() {
                    // In right-to-left mode(original Streamdeck) the first key has index 1,
                    // so we don't add the +1 here.
                    *val = cmd[offset + self.translate_key_index(i as u8)? as usize];
                }
            }
            KeyDirection::LeftToRight => {
                out[0..keys].copy_from_slice(&cmd[1 + offset..1 + offset + keys]);
            }
        }

        Ok(out)
    }

    /// Fetch image size for the connected device
    pub fn image_size(&self) -> (usize, usize) {
        self.kind.image_size()
    }

    /// Convert an image into the device dependent format
    pub fn convert_image(&self, image: Vec<u8>) -> Result<DeviceImage, Error> {
            // Check image dimensions
        if image.len() != self.kind.image_size_bytes() {
            return Err(Error::InvalidImageSize);
        }
        let image = match self.kind.image_mode() {
            ImageMode::Bmp => image,
            ImageMode::Jpeg => {
                let (w, h) = self.kind.image_size();
                encode_jpeg(&image, w, h)?
            }
        };
        Ok(DeviceImage{ data: image })
    }

    /// Set a button to the provided RGB colour
    pub fn set_button_rgb(&mut self, key: u8, colour: &Colour) -> Result<(), Error> {
        let mut image = vec![0u8; self.kind.image_size_bytes()];
        let colour_order = self.kind.image_colour_order();

        for i in 0..image.len() {
            match i % 3 {
                0 => {
                    image[i] = match colour_order {
                        ColourOrder::BGR => colour.b,
                        ColourOrder::RGB => colour.r,
                    }
                }
                1 => image[i] = colour.g,
                2 => {
                    image[i] = match colour_order {
                        ColourOrder::BGR => colour.r,
                        ColourOrder::RGB => colour.b,
                    }
                }
                _ => unreachable!(),
            };
        }
        self.write_button_image(key, &self.convert_image(image)?)?;

        Ok(())
    }

    /// Set a button to the provided image
    pub fn set_button_image(&mut self, key: u8, image: DynamicImage) -> Result<(), Error> {
        let image = apply_transform(image, self.kind.image_rotation(), self.kind.image_mirror());
        let mut data = image.into_rgb8().into_vec();
        if matches!(self.kind.image_colour_order(), ColourOrder::BGR) {
            rgb_to_bgr(&mut data);
        }
        self.write_button_image(key, &self.convert_image(data)?)
    }

    /// Sets a button to the provided text.
    /// Will break text over \n linebreaks
    pub fn set_button_text(
        &mut self,
        key: u8,
        font: &Font,
        pos: &TextPosition,
        text: &str,
        opts: &TextOptions,
    ) -> Result<(), Error> {
        let (width, height) = self.kind.image_size();
        let background = Rgb([opts.background.r, opts.background.g, opts.background.b]);
        let colour = Rgb([opts.foreground.r, opts.foreground.g, opts.foreground.b]);
        let mut image = ImageBuffer::from_pixel(width as u32, height as u32, background);

        match pos {
            TextPosition::Absolute { x, y } => {
                let mut y = *y;
                text.to_string().split("\n").for_each(|txt| {
                    draw_text_mut(&mut image, colour, *x, y, opts.scale, font, txt);
                    y += (opts.scale.y * opts.line_height).round() as i32;
                });
            }
        }

        self.set_button_image(key, DynamicImage::ImageRgb8(image))
    }

    ///  Set a button to the provided image file
    pub fn set_button_file(
        &mut self,
        key: u8,
        image: &str,
        opts: &ImageOptions,
    ) -> Result<(), Error> {

        self.write_button_image(key, &self.load_image(image, opts)?)
    }

    /// Load an image file into the device specific representation
    pub fn load_image(
        &self,
        image: &str,
        opts: &ImageOptions,
    ) -> Result<DeviceImage, Error> {
        let (x, y) = self.kind.image_size();
        let rotate = self.kind.image_rotation();
        let mirror = self.kind.image_mirror();

        let image = images::load_image(
            image,
            x,
            y,
            rotate,
            mirror,
            opts,
            self.kind.image_colour_order(),
        )?;
        self.convert_image(image)
    }

    /// Transforms a key from zero-indexed left-to-right into the device-correct coordinate system
    fn translate_key_index(&self, key: u8) -> Result<u8, Error> {
        if key > self.kind.keys() {
            return Err(Error::InvalidKeyIndex);
        }
        let mapped = match self.kind.key_direction() {
            // All but the original Streamdeck already have correct coordinates
            KeyDirection::LeftToRight => key,
            // The original Streamdeck uses 1-indexed right-to-left
            KeyDirection::RightToLeft => {
                let cols = self.kind.key_columns() as u8;
                let col = key % cols;
                let row = key / cols;
                row * cols + cols - col
            }
        };
        Ok(mapped)
    }

    /// Writes an image to a button
    /// Image at this point in correct dimensions and in device native colour order.
    pub fn write_button_image(&mut self, key: u8, image: &DeviceImage) -> Result<(), Error> {

        let image = &image.data;
        let key = self.translate_key_index(key)?;

        let mut buf = vec![0u8; self.kind.image_report_len()];
        let base = self.kind.image_base();
        let hdrlen = self.kind.image_report_header_len();

        match self.kind {
            Kind::Original => {
                // Original Streamdeck uses static lengths, not the dynamically sized protocol on the
                // later versions. First packet contains the initial 7749 bytes.
                self.write_image_header(&mut buf, key, 1, false, 0);
                let start = hdrlen + base.len();
                buf[hdrlen..start].copy_from_slice(base);
                buf[start..start + 7749].copy_from_slice(&image[0..7749]);
                self.device.write(&buf)?;

                // Second packet contains the last 7803 bytes
                self.write_image_header(&mut buf, key, 2, true, 0);
                buf[hdrlen..hdrlen + 7803].copy_from_slice(&image[7749..15552]);
                self.device.write(&buf)?;

                Ok(())
            }

            _ => {
                let mut sequence = 0;
                let mut offset = 0;
                let maxdatalen = buf.len() - hdrlen;

                while offset < image.len() {
                    let mut take = (image.len() - offset).min(maxdatalen);
                    let mut start = hdrlen;

                    if sequence == 0 && !base.is_empty() {
                        trace!("outputting base");
                        buf[start..start + base.len()].copy_from_slice(base);
                        // Recalculate take with the smaller room
                        take = (image.len() - offset).min(maxdatalen - base.len());
                        start += base.len();
                    }

                    let is_last = take == image.len() - offset;
                    self.write_image_header(&mut buf, key, sequence, is_last, take);
                    buf[start..start + take].copy_from_slice(&image[offset..offset + take]);

                    trace!(
                        "outputting image chunk [{}..{}[ in [{}..{}[, sequence {}{}",
                        offset,
                        offset + take,
                        start,
                        start + take,
                        sequence,
                        if is_last { " (last)" } else { "" },
                    );
                    self.device.write(&buf)?;

                    sequence += 1;
                    offset += take;
                }
                Ok(())
            }
        }
    }

    /// Writes the image report header to the given buffer
    fn write_image_header(
        &self,
        buf: &mut [u8],
        key: u8,
        sequence: u16,
        is_last: bool,
        payload_len: usize,
    ) {
        if self.kind.is_v2() {
            buf[0] = 0x02;
            buf[1] = 0x07;
            buf[2] = key;
            buf[3] = if is_last { 1 } else { 0 };
            buf[4..6].copy_from_slice(&(payload_len as u16).to_le_bytes());
            buf[6..8].copy_from_slice(&sequence.to_le_bytes());
        } else {
            buf[0] = 0x02;
            buf[1] = 0x01;
            buf[2..4].copy_from_slice(&sequence.to_le_bytes());
            buf[4] = if is_last { 1 } else { 0 };
            buf[5] = key;
        }
    }
}

#[derive(Debug, Clone)]
pub enum Input {
    None,
    Button(Vec<u8>),
    Touch(TouchInput),
    Knob(KnobInput),
    Other
}

#[derive(Debug, Clone)]
pub enum KnobInput {
    Press(Vec<u8>),
    Rotate(Vec<i8>),
}

#[derive(Debug, Clone)]
pub enum TouchInput {
    Short { x: u16, y: u16 },
    Long { x: u16, y: u16 },
    Swipe { x0: u16, y0: u16, x1: u16, y1: u16},
}

/// TextPosition is how to position text via set_button_text
pub enum TextPosition {
    /// Absolute positioning
    Absolute { x: i32, y: i32 },
}

/// Text Options provide values for text buttons
pub struct TextOptions {
    foreground: Colour,
    background: Colour,
    scale: Scale,
    line_height: f32,
}

impl TextOptions {
    pub fn new(foreground: Colour, background: Colour, scale: Scale, line_height: f32) -> Self {
        TextOptions {
            foreground,
            background,
            scale,
            line_height,
        }
    }
}

impl Default for TextOptions {
    /// default is white text on a black background, with 15 pixel high text
    /// and 1.1x the line height.
    fn default() -> Self {
        TextOptions {
            foreground: Colour::from_str("FFFFFF").unwrap(),
            background: Colour::from_str("000000").unwrap(),
            scale: Scale { x: 15.0, y: 15.0 },
            line_height: 1.1,
        }
    }
}

// Convert RGB image data to BGR
fn rgb_to_bgr(data: &mut Vec<u8>) {
    for chunk in data.chunks_exact_mut(3) {
        chunk.swap(0, 2);
    }
}
