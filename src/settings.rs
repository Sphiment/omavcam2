//! Per-phone camera capabilities and the settings fixed at capture launch.

use std::collections::BTreeMap;
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::command;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lens {
    pub id: String,
    pub facing: String,
    pub sensor_size: String,
    pub resolutions: Vec<String>,
    pub frame_rates: Vec<u32>,
    pub zoom_min: f64,
    pub zoom_max: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Crop {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraSettings {
    pub lens: String,
    pub resolution: String,
    pub frame_rate: u32,
    pub aspect_ratio: String,
    pub zoom: f64,
    #[serde(default)]
    pub crops: BTreeMap<String, Crop>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsState {
    pub phone: String,
    pub lenses: Vec<Lens>,
    pub applied: CameraSettings,
    pub pending: CameraSettings,
    pub offered_resolutions: Vec<String>,
    pub has_pending_changes: bool,
    pub rejected: Option<String>,
}

impl SettingsState {
    pub fn new(phone: String, lenses: Vec<Lens>, saved: Option<CameraSettings>) -> Self {
        let applied = saved
            .filter(|settings| valid(settings, &lenses))
            .unwrap_or_else(|| defaults(&lenses));
        let mut state = Self {
            phone,
            lenses,
            pending: applied.clone(),
            applied,
            offered_resolutions: Vec::new(),
            has_pending_changes: false,
            rejected: None,
        };
        state.refresh();
        state
    }

    pub fn change(&mut self, name: &str, value: &Value) -> Result<(), String> {
        match name {
            "lens" => {
                let id = string(value)?;
                let lens = self
                    .lenses
                    .iter()
                    .find(|lens| lens.id == id)
                    .ok_or_else(|| format!("lens {id:?} is not available"))?;
                self.pending.lens = id.to_string();
                if !lens.resolutions.contains(&self.pending.resolution) {
                    self.pending.resolution = preferred(&lens.resolutions).to_string();
                }
                self.pending.aspect_ratio = aspect_ratio(&self.pending.resolution)?;
                if !lens.frame_rates.contains(&self.pending.frame_rate) {
                    self.pending.frame_rate = preferred_fps(&lens.frame_rates);
                }
                self.pending.zoom = self.pending.zoom.clamp(lens.zoom_min, lens.zoom_max);
            }
            "resolution" => {
                let resolution = string(value)?;
                let lens = self.lens()?;
                if !lens.resolutions.iter().any(|size| size == resolution)
                    || aspect_ratio(resolution)? != self.pending.aspect_ratio
                {
                    return Err(format!(
                        "resolution {resolution:?} is not offered for lens {} at {}",
                        lens.id, self.pending.aspect_ratio
                    ));
                }
                self.pending.resolution = resolution.to_string();
            }
            "frame_rate" => {
                let frame_rate = value
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or_else(|| "frame_rate must be a positive integer".to_string())?;
                if !self.lens()?.frame_rates.contains(&frame_rate) {
                    return Err(format!(
                        "frame rate {frame_rate} is not reported by lens {}",
                        self.pending.lens
                    ));
                }
                self.pending.frame_rate = frame_rate;
            }
            "aspect_ratio" => {
                let ratio = string(value)?;
                let offered = self
                    .lens()?
                    .resolutions
                    .iter()
                    .filter(|size| aspect_ratio(size).as_deref() == Ok(ratio))
                    .cloned()
                    .collect::<Vec<_>>();
                if offered.is_empty() {
                    return Err(format!(
                        "aspect ratio {ratio:?} is not offered by lens {}",
                        self.pending.lens
                    ));
                }
                self.pending.aspect_ratio = ratio.to_string();
                if !offered.contains(&self.pending.resolution) {
                    self.pending.resolution = preferred(&offered).to_string();
                }
            }
            "zoom" => {
                let zoom = value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| "zoom must be a number".to_string())?;
                let lens = self.lens()?;
                if !(lens.zoom_min..=lens.zoom_max).contains(&zoom) {
                    return Err(format!(
                        "zoom {zoom} is outside lens {}'s range [{}, {}]",
                        lens.id, lens.zoom_min, lens.zoom_max
                    ));
                }
                self.pending.zoom = zoom;
            }
            "crop" if value.is_null() => {
                self.pending.crops.remove(&self.pending.lens);
            }
            "crop" => {
                let crop: Crop = serde_json::from_value(value.clone())
                    .map_err(|_| "crop must contain numeric x, y, width and height".to_string())?;
                if ![crop.x, crop.y, crop.width, crop.height]
                    .iter()
                    .all(|value| value.is_finite())
                    || crop.x < 0.0
                    || crop.y < 0.0
                    || crop.x >= 1.0
                    || crop.y >= 1.0
                    || crop.width <= 0.0
                    || crop.height <= 0.0
                {
                    return Err("crop coordinates must be finite normalized values".to_string());
                }
                self.pending.crops.insert(self.pending.lens.clone(), crop);
            }
            _ => return Err(format!("no such camera setting: {name:?}")),
        }
        self.rejected = None;
        self.refresh();
        Ok(())
    }

    pub fn discard(&mut self) {
        self.pending = self.applied.clone();
        self.rejected = None;
        self.refresh();
    }

    pub fn applied(&mut self) {
        self.applied = self.pending.clone();
        self.rejected = None;
        self.refresh();
    }

    pub fn reject(&mut self, message: String) {
        self.pending = self.applied.clone();
        self.rejected = Some(message);
        self.refresh();
    }

    pub fn note_rejection(&mut self, message: String) {
        self.rejected = Some(message);
        self.refresh();
    }

    fn lens(&self) -> Result<&Lens, String> {
        self.lenses
            .iter()
            .find(|lens| lens.id == self.pending.lens)
            .ok_or_else(|| format!("lens {:?} is no longer available", self.pending.lens))
    }

    fn refresh(&mut self) {
        self.offered_resolutions = self
            .lenses
            .iter()
            .find(|lens| lens.id == self.pending.lens)
            .into_iter()
            .flat_map(|lens| lens.resolutions.iter())
            .filter(|size| aspect_ratio(size).as_deref() == Ok(self.pending.aspect_ratio.as_str()))
            .cloned()
            .collect();
        self.has_pending_changes = self.pending != self.applied;
    }
}

pub fn inspect(serial: &str) -> Result<Vec<Lens>, String> {
    let mut process = Command::new("scrcpy");
    process.args(["-s", serial, "--video-source=camera", "--list-camera-sizes"]);
    let output =
        command::output(process).map_err(|error| format!("could not run scrcpy: {error}"))?;
    if !output.status.success() {
        return Err(format!("scrcpy could not list cameras ({})", output.status));
    }
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    parse(&text)
}

fn parse(text: &str) -> Result<Vec<Lens>, String> {
    let mut lenses = Vec::new();
    let mut current: Option<Lens> = None;
    let mut high_speed = false;
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("--camera-id=") {
            if let Some(lens) = current.take() {
                lenses.push(lens);
            }
            current = Some(parse_lens(rest)?);
            high_speed = false;
        } else if line.starts_with("High speed capture") {
            high_speed = true;
        } else if !high_speed {
            if let Some(size) = line.strip_prefix("- ") {
                parse_size(size).ok_or_else(|| format!("invalid lens resolution {size:?}"))?;
                current
                    .as_mut()
                    .ok_or_else(|| format!("lens resolution without a lens: {size:?}"))?
                    .resolutions
                    .push(size.to_string());
            }
        }
    }
    if let Some(lens) = current {
        lenses.push(lens);
    }
    if lenses.is_empty() || lenses.iter().any(|lens| lens.resolutions.is_empty()) {
        return Err("scrcpy returned no usable camera capabilities".to_string());
    }
    Ok(lenses)
}

fn parse_lens(rest: &str) -> Result<Lens, String> {
    let id_end = rest
        .find(char::is_whitespace)
        .ok_or_else(|| format!("could not parse lens id from {rest:?}"))?;
    let id = rest[..id_end].to_string();
    if id.is_empty() {
        return Err(format!("empty lens id in {rest:?}"));
    }
    let details = rest[id_end..]
        .trim()
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
        .ok_or_else(|| format!("could not parse lens characteristics from {rest:?}"))?;
    let mut fields = details.split(',').map(str::trim);
    let facing = fields.next().unwrap_or_default().to_string();
    if facing.is_empty() {
        return Err(format!("empty lens facing in {rest:?}"));
    }
    let sensor_size = fields.next().unwrap_or_default().to_string();
    parse_size(&sensor_size).ok_or_else(|| format!("invalid sensor size {sensor_size:?}"))?;
    let frame_rates = between(details, "fps={", "}")?
        .split(',')
        .map(|value| value.trim().parse::<u32>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| format!("invalid frame rates in {rest:?}"))?;
    let zoom = between(details, "zoom-range=[", "]")?
        .split(',')
        .map(|value| value.trim().parse::<f64>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| format!("invalid zoom range in {rest:?}"))?;
    if zoom.len() != 2
        || frame_rates.is_empty()
        || frame_rates.contains(&0)
        || zoom.iter().any(|value| !value.is_finite() || *value <= 0.0)
        || zoom[0] > zoom[1]
    {
        return Err(format!("incomplete camera characteristics in {rest:?}"));
    }
    Ok(Lens {
        id,
        facing,
        sensor_size,
        resolutions: Vec::new(),
        frame_rates,
        zoom_min: zoom[0],
        zoom_max: zoom[1],
    })
}

fn between<'a>(text: &'a str, before: &str, after: &str) -> Result<&'a str, String> {
    text.split_once(before)
        .and_then(|(_, rest)| rest.split_once(after))
        .map(|(value, _)| value)
        .ok_or_else(|| format!("missing {before} in {text:?}"))
}

fn defaults(lenses: &[Lens]) -> CameraSettings {
    let lens = &lenses[0];
    let resolution = preferred(&lens.resolutions).to_string();
    CameraSettings {
        lens: lens.id.clone(),
        aspect_ratio: aspect_ratio(&resolution).expect("scrcpy resolution was parsed"),
        resolution,
        frame_rate: preferred_fps(&lens.frame_rates),
        zoom: lens.zoom_min,
        crops: BTreeMap::new(),
    }
}

fn valid(settings: &CameraSettings, lenses: &[Lens]) -> bool {
    lenses
        .iter()
        .find(|lens| lens.id == settings.lens)
        .is_some_and(|lens| {
            lens.resolutions.contains(&settings.resolution)
                && lens.frame_rates.contains(&settings.frame_rate)
                && (lens.zoom_min..=lens.zoom_max).contains(&settings.zoom)
                && aspect_ratio(&settings.resolution).as_deref()
                    == Ok(settings.aspect_ratio.as_str())
        })
}

fn preferred(values: &[String]) -> &str {
    values
        .iter()
        .find(|value| value.as_str() == "1280x720")
        .unwrap_or(&values[0])
}

fn preferred_fps(values: &[u32]) -> u32 {
    values
        .iter()
        .copied()
        .find(|value| *value == 30)
        .unwrap_or(values[0])
}

fn string(value: &Value) -> Result<&str, String> {
    value
        .as_str()
        .ok_or_else(|| "setting value must be a string".to_string())
}

fn parse_size(size: &str) -> Option<(u32, u32)> {
    let (width, height) = size.split_once('x')?;
    let size = (width.parse().ok()?, height.parse().ok()?);
    (size.0 >= 2 && size.1 >= 2).then_some(size)
}

fn aspect_ratio(size: &str) -> Result<String, String> {
    let (width, height) = parse_size(size).ok_or_else(|| format!("invalid resolution {size:?}"))?;
    let divisor = gcd(width, height);
    Ok(format!("{}:{}", width / divisor, height / divisor))
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

pub fn crop_pixels(settings: &CameraSettings) -> Option<(u32, u32, u32, u32)> {
    let crop = settings.crops.get(&settings.lens)?;
    let (frame_width, frame_height) = parse_size(&settings.resolution)?;
    let x = even_down((crop.x.clamp(0.0, 1.0) * frame_width as f64).floor() as u32)
        .min(frame_width.saturating_sub(2));
    let y = even_down((crop.y.clamp(0.0, 1.0) * frame_height as f64).floor() as u32)
        .min(frame_height.saturating_sub(2));
    let right = ((crop.x + crop.width).clamp(0.0, 1.0) * frame_width as f64).round() as u32;
    let bottom = ((crop.y + crop.height).clamp(0.0, 1.0) * frame_height as f64).round() as u32;
    let width = even_down(right.saturating_sub(x))
        .max(2)
        .min(even_down(frame_width - x));
    let height = even_down(bottom.saturating_sub(y))
        .max(2)
        .min(even_down(frame_height - y));
    Some((width, height, x, y))
}

fn even_down(value: u32) -> u32 {
    value & !1
}

pub fn output_size(settings: &CameraSettings) -> String {
    crop_pixels(settings)
        .map(|(width, height, _, _)| format!("{width}x{height}"))
        .unwrap_or_else(|| settings.resolution.clone())
}
