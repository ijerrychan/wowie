use super::{
    results::{icon_to_image, static_icon_kind},
    ContextActionRow, DetailBlock, Launcher,
};
use crate::{
    icon_service::IconService,
    result::{DetailBlock as ResultDetailBlock, DetailView, SearchResult},
};
use slint::{ModelRc, VecModel};
use std::{
    rc::Rc,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

pub(super) fn request_dynamic_detail(
    item: SearchResult,
    registry: crate::details::DetailRegistry,
    language: String,
    generation: Arc<AtomicU64>,
    weak: slint::Weak<Launcher>,
    icon_service: IconService,
) {
    let request_id = generation.fetch_add(1, Ordering::AcqRel) + 1;
    request_dynamic_detail_with_id(
        item,
        registry,
        language,
        generation,
        weak,
        request_id,
        icon_service,
    );
}

pub(super) fn request_dynamic_detail_with_id(
    item: SearchResult,
    registry: crate::details::DetailRegistry,
    language: String,
    generation: Arc<AtomicU64>,
    weak: slint::Weak<Launcher>,
    request_id: u64,
    icon_service: IconService,
) {
    let Some(provider) = registry.provider_for(&item) else {
        return;
    };
    if let Some(cached) = provider.cached(&language) {
        if let Some(window) = weak.upgrade() {
            set_detail_view(&window, Some(&cached), &icon_service);
            window.set_detail_panel_visible(true);
        }
        return;
    }

    if let Some(window) = weak.upgrade() {
        let loading = DetailView {
            title: item.title.clone(),
            subtitle: String::new(),
            source: String::new(),
            blocks: vec![ResultDetailBlock::Loading {
                message: crate::utils::i18n::tr(&language, "Loading preview").into(),
            }],
            actions: Vec::new(),
            preferred_width: None,
            scrollable: false,
        };
        set_detail_view(&window, Some(&loading), &icon_service);
        window.set_detail_panel_visible(true);
    }

    std::thread::spawn(move || {
        let result = provider.load(&language);
        let _ = slint::invoke_from_event_loop(move || {
            if generation.load(Ordering::Acquire) != request_id {
                return;
            }
            let Some(window) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(detail) => {
                    set_detail_view(&window, Some(&detail), &icon_service);
                    window.set_detail_panel_visible(true);
                }
                Err(error) => {
                    eprintln!("dynamic detail request failed: {error:#}");
                    let detail = provider.error_view(&language);
                    set_detail_view(&window, Some(&detail), &icon_service);
                    window.set_detail_panel_visible(true);
                }
            }
        });
    });
}

pub(super) fn set_detail_view(
    window: &Launcher,
    view: Option<&DetailView>,
    icon_service: &IconService,
) {
    if let Some(view) = view {
        window.set_detail_title(view.title.clone().into());
        window.set_detail_subtitle(view.subtitle.clone().into());
        window.set_detail_source(view.source.clone().into());
        window.set_detail_panel_preferred_width(view.preferred_width.unwrap_or(0));
        window.set_detail_panel_scrollable(view.scrollable);
        window.set_detail_blocks(detail_blocks_model(view, icon_service));
        window.set_detail_actions(detail_actions_model(view, icon_service));
    } else {
        window.set_detail_title(String::new().into());
        window.set_detail_subtitle(String::new().into());
        window.set_detail_source(String::new().into());
        window.set_detail_panel_preferred_width(0);
        window.set_detail_panel_scrollable(false);
        window.set_detail_blocks(ModelRc::from(Rc::new(VecModel::from(
            Vec::<DetailBlock>::new(),
        ))));
        window.set_detail_actions(ModelRc::from(Rc::new(VecModel::from(Vec::<
            ContextActionRow,
        >::new()))));
    }
}

fn detail_blocks_model(view: &DetailView, icon_service: &IconService) -> ModelRc<DetailBlock> {
    let blocks = view
        .blocks
        .iter()
        .map(|block| match block {
            ResultDetailBlock::Text { text, color } => DetailBlock {
                kind: "text".into(),
                label: String::new().into(),
                value: text.clone().into(),
                color: color.clone().into(),
                secondary: String::new().into(),
                icon: slint::Image::default(),
            },
            ResultDetailBlock::Metric {
                label,
                value,
                color,
            } => DetailBlock {
                kind: "metric".into(),
                label: label.clone().into(),
                value: value.clone().into(),
                color: color.clone().into(),
                secondary: String::new().into(),
                icon: slint::Image::default(),
            },
            ResultDetailBlock::Icon { glyph, color, icon } => DetailBlock {
                kind: "icon".into(),
                label: String::new().into(),
                value: glyph.clone().into(),
                color: color.clone().into(),
                secondary: String::new().into(),
                icon: icon
                    .as_ref()
                    .and_then(|icon| icon_service.resolve_spec_at(icon, 40))
                    .map(|icon| icon_to_image(icon.as_ref()))
                    .unwrap_or_default(),
            },
            ResultDetailBlock::Weather { value } => DetailBlock {
                kind: "weather".into(),
                label: String::new().into(),
                value: value.clone().into(),
                color: String::new().into(),
                secondary: String::new().into(),
                icon: slint::Image::default(),
            },
            ResultDetailBlock::Markdown { markdown, color } => DetailBlock {
                kind: "markdown".into(),
                label: String::new().into(),
                value: markdown.clone().into(),
                color: color.clone().into(),
                secondary: String::new().into(),
                icon: slint::Image::default(),
            },
            ResultDetailBlock::Image { source, alt } => DetailBlock {
                kind: "image".into(),
                label: alt.clone().into(),
                value: source.clone().into(),
                color: String::new().into(),
                secondary: alt.clone().into(),
                icon: slint::Image::load_from_path(std::path::Path::new(source))
                    .unwrap_or_default(),
            },
            ResultDetailBlock::Code { language, code } => DetailBlock {
                kind: "code".into(),
                label: language.clone().into(),
                value: code.clone().into(),
                color: String::new().into(),
                secondary: String::new().into(),
                icon: slint::Image::default(),
            },
            ResultDetailBlock::Table { columns, rows } => DetailBlock {
                kind: "table".into(),
                label: columns.join("  ·  ").into(),
                value: rows
                    .iter()
                    .map(|row| row.join("    "))
                    .collect::<Vec<_>>()
                    .join("\n")
                    .into(),
                color: String::new().into(),
                secondary: String::new().into(),
                icon: slint::Image::default(),
            },
            ResultDetailBlock::Link { label, url } => DetailBlock {
                kind: "link".into(),
                label: label.clone().into(),
                value: url.clone().into(),
                color: "accent".into(),
                secondary: String::new().into(),
                icon: slint::Image::default(),
            },
            ResultDetailBlock::Loading { message } => DetailBlock {
                kind: "loading".into(),
                label: String::new().into(),
                value: message.clone().into(),
                color: String::new().into(),
                secondary: String::new().into(),
                icon: slint::Image::default(),
            },
            ResultDetailBlock::Error {
                message,
                retry_action,
            } => DetailBlock {
                kind: "error".into(),
                label: retry_action.clone().unwrap_or_default().into(),
                value: message.clone().into(),
                color: String::new().into(),
                secondary: String::new().into(),
                icon: slint::Image::default(),
            },
            ResultDetailBlock::Separator => DetailBlock {
                kind: "separator".into(),
                label: String::new().into(),
                value: String::new().into(),
                color: String::new().into(),
                secondary: String::new().into(),
                icon: slint::Image::default(),
            },
        })
        .collect::<Vec<_>>();
    ModelRc::from(Rc::new(VecModel::from(blocks)))
}

fn detail_actions_model(
    view: &DetailView,
    icon_service: &IconService,
) -> ModelRc<ContextActionRow> {
    let actions = view
        .actions
        .iter()
        .take(3)
        .map(|action| ContextActionRow {
            id: action.id.clone().into(),
            title: action.title.clone().into(),
            subtitle: String::new().into(),
            kind: "detail".into(),
            icon: action
                .icon
                .as_ref()
                .filter(|icon| static_icon_kind(Some(icon)).is_empty())
                .and_then(|icon| icon_service.resolve_spec_at(icon, 16))
                .map(|icon| icon_to_image(icon.as_ref()))
                .unwrap_or_default(),
            icon_kind: static_icon_kind(action.icon.as_ref()).into(),
            shortcut: String::new().into(),
        })
        .collect::<Vec<_>>();
    ModelRc::from(Rc::new(VecModel::from(actions)))
}

#[cfg(any())]
fn weather_detail_view(data: &crate::details::weather::WeatherData, language: &str) -> DetailView {
    DetailView {
        title: crate::utils::i18n::tr(language, "Weather").into(),
        subtitle: data.location.clone(),
        source: format!(
            "{}: Open-Meteo · {}",
            crate::utils::i18n::tr(language, "Source"),
            data.source
        ),
        actions: Vec::new(),
        blocks: vec![
            ResultDetailBlock::Weather {
                value: match data.weather_code {
                    0 => "clear",
                    71..=77 | 85..=86 => "snow",
                    51..=67 | 80..=82 => "rain",
                    95..=99 => "storm",
                    _ => "cloud",
                }
                .into(),
            },
            ResultDetailBlock::Text {
                text: format!("{}  {}", data.temperature, data.condition),
                color: "primary".into(),
            },
            ResultDetailBlock::Separator,
            ResultDetailBlock::Metric {
                label: crate::utils::i18n::tr(language, "Humidity").into(),
                value: data.humidity.clone(),
                color: "secondary".into(),
            },
            ResultDetailBlock::Metric {
                label: crate::utils::i18n::tr(language, "Wind").into(),
                value: data.wind.clone(),
                color: "secondary".into(),
            },
        ],
        preferred_width: None,
        scrollable: false,
    }
}
