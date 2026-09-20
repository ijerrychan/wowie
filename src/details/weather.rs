use anyhow::{Context, Result};
use serde::Deserialize;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const CACHE_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone)]
pub struct WeatherData {
    pub location: String,
    pub temperature: String,
    pub weather_code: i32,
    pub condition: String,
    pub humidity: String,
    pub wind: String,
    pub source: String,
}

/// Short-lived in-memory weather cache shared by the UI callbacks and the
/// weather worker. The cache belongs here rather than in `main`, because it
/// is part of the weather provider's data policy, not window state.
#[derive(Clone, Default)]
pub struct WeatherCache {
    value: Arc<Mutex<Option<(Instant, WeatherData)>>>,
}

pub struct WeatherProvider {
    cache: WeatherCache,
}

impl WeatherProvider {
    pub fn new() -> Self {
        Self {
            cache: WeatherCache::new(),
        }
    }
}

impl crate::details::DynamicDetailProvider for WeatherProvider {
    fn matches(&self, item: &crate::result::SearchResult) -> bool {
        item.id == "shortcut:weather-test"
    }

    fn cached(&self, language: &str) -> Option<crate::result::DetailView> {
        self.cache.get().map(|data| detail_view(&data, language))
    }

    fn load(&self, language: &str) -> Result<crate::result::DetailView> {
        let data = if let Some(data) = self.cache.get() {
            data
        } else {
            let data = fetch(language)?;
            self.cache.store(data.clone());
            data
        };
        Ok(detail_view(&data, language))
    }

    fn error_view(&self, language: &str) -> crate::result::DetailView {
        error_detail_view(language)
    }
}

pub fn register(registry: &mut crate::details::DetailRegistry) {
    registry.register(Arc::new(WeatherProvider::new()));
}

impl WeatherCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self) -> Option<WeatherData> {
        let cache = self.value.lock().ok()?;
        let (created, data) = cache.as_ref()?;
        (created.elapsed() < CACHE_TTL).then(|| data.clone())
    }

    pub fn store(&self, data: WeatherData) {
        if let Ok(mut cache) = self.value.lock() {
            *cache = Some((Instant::now(), data));
        }
    }
}

#[derive(Debug, Clone)]
struct Coordinates {
    latitude: f64,
    longitude: f64,
    location: String,
    source: String,
}

#[derive(Debug, Deserialize)]
struct IpLocation {
    city: Option<String>,
    #[serde(alias = "country")]
    country_name: Option<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ForecastResponse {
    current: CurrentWeather,
}

#[derive(Debug, Deserialize)]
struct CurrentWeather {
    temperature_2m: f64,
    relative_humidity_2m: f64,
    weather_code: i32,
    wind_speed_10m: f64,
    wind_direction_10m: f64,
}

pub fn fetch(language: &str) -> Result<WeatherData> {
    // Keep weather fully in-process. A PowerShell-based location probe could
    // steal focus in release builds and trigger the launcher's hide behavior.
    let coordinates = ip_location(language)?;
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={:.5}&longitude={:.5}&current=temperature_2m,relative_humidity_2m,weather_code,wind_speed_10m,wind_direction_10m&temperature_unit=celsius&wind_speed_unit=kmh&timezone=auto",
        coordinates.latitude, coordinates.longitude
    );
    let forecast: ForecastResponse =
        crate::utils::http::get_json(&url).context("request Open-Meteo weather")?;
    let current = forecast.current;
    Ok(WeatherData {
        location: coordinates.location,
        temperature: format!("{:.0}°", current.temperature_2m),
        weather_code: current.weather_code,
        condition: crate::utils::i18n::tr(language, weather_condition(current.weather_code)).into(),
        humidity: format!("{:.0}%", current.relative_humidity_2m),
        wind: format!(
            "{:.0} km/h {}",
            current.wind_speed_10m,
            wind_direction(current.wind_direction_10m)
        ),
        source: coordinates.source,
    })
}

pub fn detail_view(data: &WeatherData, language: &str) -> crate::result::DetailView {
    crate::result::DetailView {
        title: crate::utils::i18n::tr(language, "Weather").into(),
        subtitle: data.location.clone(),
        source: format!(
            "{}: Open-Meteo · {}",
            crate::utils::i18n::tr(language, "Source"),
            data.source
        ),
        actions: Vec::new(),
        blocks: vec![
            crate::result::DetailBlock::Weather {
                value: visual_kind(data.weather_code).into(),
            },
            crate::result::DetailBlock::Text {
                text: format!("{}  {}", data.temperature, data.condition),
                color: "primary".into(),
            },
            crate::result::DetailBlock::Separator,
            crate::result::DetailBlock::Metric {
                label: crate::utils::i18n::tr(language, "Humidity").into(),
                value: data.humidity.clone(),
                color: "secondary".into(),
            },
            crate::result::DetailBlock::Metric {
                label: crate::utils::i18n::tr(language, "Wind").into(),
                value: data.wind.clone(),
                color: "secondary".into(),
            },
        ],
        preferred_width: None,
        scrollable: false,
    }
}

pub fn error_detail_view(language: &str) -> crate::result::DetailView {
    crate::result::DetailView {
        title: crate::utils::i18n::tr(language, "Weather").into(),
        subtitle: crate::utils::i18n::tr(language, "Current location").into(),
        source: format!(
            "{}: {}",
            crate::utils::i18n::tr(language, "Source"),
            crate::utils::i18n::tr(language, "Unavailable")
        ),
        actions: Vec::new(),
        blocks: vec![crate::result::DetailBlock::Text {
            text: crate::utils::i18n::tr(language, "Weather unavailable").into(),
            color: "secondary".into(),
        }],
        preferred_width: None,
        scrollable: false,
    }
}

fn visual_kind(code: i32) -> &'static str {
    match code {
        0 => "clear",
        71..=77 | 85..=86 => "snow",
        51..=67 | 80..=82 => "rain",
        95..=99 => "storm",
        _ => "cloud",
    }
}

fn ip_location(language: &str) -> Result<Coordinates> {
    let value: IpLocation = crate::utils::http::get_json("https://ipapi.co/json/")
        .or_else(|_| crate::utils::http::get_json("https://ipwho.is/"))
        .context("all IP location services failed")?;
    let city = value.city.unwrap_or_else(|| "Approximate location".into());
    let country = value.country_name.unwrap_or_default();
    let label = if country.is_empty() {
        city
    } else {
        format!("{city}, {country}")
    };
    Ok(Coordinates {
        latitude: value.latitude.context("IP location returned no latitude")?,
        longitude: value
            .longitude
            .context("IP location returned no longitude")?,
        location: label,
        source: crate::utils::i18n::tr(language, "IP location").into(),
    })
}

fn weather_condition(code: i32) -> &'static str {
    match code {
        0 => "Clear sky",
        1..=3 => "Partly cloudy",
        45 | 48 => "Foggy",
        51..=57 => "Drizzle",
        61..=67 => "Rain",
        71..=77 => "Snow",
        80..=82 => "Rain showers",
        85 | 86 => "Snow showers",
        95 => "Thunderstorm",
        96 | 99 => "Thunderstorm with hail",
        _ => "Unknown conditions",
    }
}

fn wind_direction(degrees: f64) -> &'static str {
    [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW",
        "NW", "NNW",
    ][((degrees.rem_euclid(360.0) + 11.25) / 22.5) as usize % 16]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weather_codes_have_human_readable_labels() {
        assert_eq!(weather_condition(0), "Clear sky");
        assert_eq!(weather_condition(63), "Rain");
        assert_eq!(weather_condition(95), "Thunderstorm");
    }

    #[test]
    fn wind_direction_wraps_to_compass_points() {
        assert_eq!(wind_direction(0.0), "N");
        assert_eq!(wind_direction(90.0), "E");
        assert_eq!(wind_direction(270.0), "W");
    }
}
