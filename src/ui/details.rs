use super::{
    results::{icon_to_image, static_icon_kind},
    ContextActionRow, DetailBlock, Launcher,
};
use crate::{
    icon_service::IconService,
    result::{DetailBlock as ResultDetailBlock, DetailView, IconSpec, SearchResult},
};
use slint::{ComponentHandle, Model, ModelRc, Timer, VecModel};
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
    if let Some(cached) = provider.cached(&item, &language) {
        if let Some(window) = weak.upgrade() {
            set_detail_view(&window, Some(&cached), &icon_service);
            window.set_detail_panel_visible(true);
        }
        return;
    }

    if let Some(window) = weak.upgrade() {
        // Clear the previous item's data immediately, but leave the detail
        // surface in place. The loading row appears only if this request is
        // still unresolved after the same delay used by search feedback.
        let panel_width = window.get_detail_panel_preferred_width();
        set_detail_view(&window, None, &icon_service);
        window.set_detail_panel_preferred_width(panel_width);
        window.set_detail_panel_visible(true);
        let content_generation = window.get_detail_content_generation();
        let loading = DetailView {
            title: item.title.clone(),
            subtitle: String::new(),
            source: String::new(),
            blocks: vec![ResultDetailBlock::Loading {
                message: crate::utils::i18n::tr(&language, "Loading preview").into(),
            }],
            actions: Vec::new(),
            preferred_width: (panel_width > 0).then_some(panel_width),
            scrollable: false,
        };
        let weak = weak.clone();
        let generation = Arc::clone(&generation);
        let icon_service_for_feedback = icon_service.clone();
        Timer::single_shot(super::SEARCH_FEEDBACK_DELAY, move || {
            if generation.load(Ordering::Acquire) != request_id {
                return;
            }
            let Some(window) = weak.upgrade() else {
                return;
            };
            if window.get_detail_content_generation() == content_generation {
                set_detail_view(&window, Some(&loading), &icon_service_for_feedback);
                window.set_detail_panel_visible(true);
            }
        });
    }

    let task_cancellation = crate::utils::background::CancellationToken::new();
    crate::utils::background::spawn_cancellable(task_cancellation, move || {
        if generation.load(Ordering::Acquire) != request_id {
            return;
        }
        let result = provider.load(&item, &language);
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
                    let detail = provider.error_view(&item, &language);
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
    let generation = window.get_detail_content_generation().wrapping_add(1);
    window.set_detail_content_generation(generation);
    if let Some(view) = view {
        window.set_detail_title(view.title.clone().into());
        window.set_detail_subtitle(view.subtitle.clone().into());
        window.set_detail_source(view.source.clone().into());
        window.set_detail_panel_preferred_width(view.preferred_width.unwrap_or(0));
        window.set_detail_panel_scrollable(view.scrollable);
        window.set_detail_content_height(detail_content_height(view) as f32);
        window.set_detail_blocks(detail_blocks_model(view));
        window.set_detail_actions(detail_actions_model(view));
        request_detail_icons(window, view, icon_service, generation);
    } else {
        window.set_detail_title(String::new().into());
        window.set_detail_subtitle(String::new().into());
        window.set_detail_source(String::new().into());
        window.set_detail_panel_preferred_width(0);
        window.set_detail_content_height(0.0);
        window.set_detail_panel_scrollable(false);
        window.set_detail_blocks(ModelRc::from(Rc::new(VecModel::from(
            Vec::<DetailBlock>::new(),
        ))));
        window.set_detail_actions(ModelRc::from(Rc::new(VecModel::from(Vec::<
            ContextActionRow,
        >::new()))));
    }
}

fn detail_blocks_model(view: &DetailView) -> ModelRc<DetailBlock> {
    let blocks = view
        .blocks
        .iter()
        .map(|block| match block {
            ResultDetailBlock::Text {
                text,
                color,
                copyable,
            } => DetailBlock {
                kind: if *copyable { "text-copyable" } else { "text" }.into(),
                label: String::new().into(),
                value: text.clone().into(),
                color: color.clone().into(),
                secondary: String::new().into(),
                icon: slint::Image::default(),
                height: detail_block_height(block),
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
                height: detail_block_height(block),
            },
            ResultDetailBlock::Icon { glyph, color, .. } => DetailBlock {
                kind: "icon".into(),
                label: String::new().into(),
                value: glyph.clone().into(),
                color: color.clone().into(),
                secondary: String::new().into(),
                icon: slint::Image::default(),
                height: detail_block_height(block),
            },
            ResultDetailBlock::Weather { value } => DetailBlock {
                kind: "weather".into(),
                label: String::new().into(),
                value: value.clone().into(),
                color: String::new().into(),
                secondary: String::new().into(),
                icon: slint::Image::default(),
                height: detail_block_height(block),
            },
            ResultDetailBlock::Markdown {
                markdown,
                color,
                copyable,
            } => DetailBlock {
                kind: if *copyable {
                    "markdown-copyable"
                } else {
                    "markdown"
                }
                .into(),
                label: String::new().into(),
                value: markdown.clone().into(),
                color: color.clone().into(),
                secondary: String::new().into(),
                icon: slint::Image::default(),
                height: detail_block_height(block),
            },
            ResultDetailBlock::Image { source, alt } => DetailBlock {
                kind: "image".into(),
                label: alt.clone().into(),
                value: source.clone().into(),
                color: String::new().into(),
                secondary: alt.clone().into(),
                icon: slint::Image::default(),
                height: detail_block_height(block),
            },
            ResultDetailBlock::Code { language, code } => DetailBlock {
                kind: "code".into(),
                label: language.clone().into(),
                value: code.clone().into(),
                color: String::new().into(),
                secondary: String::new().into(),
                icon: slint::Image::default(),
                height: detail_block_height(block),
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
                height: detail_block_height(block),
            },
            ResultDetailBlock::Link { label, url } => DetailBlock {
                kind: "link".into(),
                label: label.clone().into(),
                value: url.clone().into(),
                color: "accent".into(),
                secondary: String::new().into(),
                icon: slint::Image::default(),
                height: detail_block_height(block),
            },
            ResultDetailBlock::Loading { message } => DetailBlock {
                kind: "loading".into(),
                label: String::new().into(),
                value: message.clone().into(),
                color: String::new().into(),
                secondary: String::new().into(),
                icon: slint::Image::default(),
                height: detail_block_height(block),
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
                height: detail_block_height(block),
            },
            ResultDetailBlock::Separator => DetailBlock {
                kind: "separator".into(),
                label: String::new().into(),
                value: String::new().into(),
                color: String::new().into(),
                secondary: String::new().into(),
                icon: slint::Image::default(),
                height: detail_block_height(block),
            },
        })
        .collect::<Vec<_>>();
    ModelRc::from(Rc::new(VecModel::from(blocks)))
}

fn detail_block_height(block: &ResultDetailBlock) -> i32 {
    fn wrapped_lines(text: &str, width: usize) -> i32 {
        let lines = text
            .lines()
            .map(|line| line.chars().count().max(1).div_ceil(width))
            .sum::<usize>()
            .max(1);
        // CJK characters occupy more width than ASCII; overestimate slightly
        // so wrapped translations are not clipped. Long content can scroll.
        (lines as i32 * 18 + 8).max(24)
    }
    match block {
        ResultDetailBlock::Text { text, .. } => wrapped_lines(text, 28),
        ResultDetailBlock::Markdown { markdown, .. } => wrapped_lines(markdown, 28),
        ResultDetailBlock::Code { code, .. } => code.lines().count().max(1) as i32 * 16 + 28,
        ResultDetailBlock::Table { rows, .. } => rows.len().max(1) as i32 * 20 + 28,
        ResultDetailBlock::Metric { .. } => 48,
        ResultDetailBlock::Weather { .. } => 48,
        ResultDetailBlock::Icon { .. } => 40,
        ResultDetailBlock::Image { .. } => 120,
        ResultDetailBlock::Link { label, url } => wrapped_lines(&format!("{label} {url}"), 28),
        ResultDetailBlock::Loading { message } => wrapped_lines(message, 28),
        ResultDetailBlock::Error { message, .. } => wrapped_lines(message, 28),
        ResultDetailBlock::Separator => 1,
    }
}

fn detail_content_height(view: &DetailView) -> i32 {
    view.blocks.iter().map(detail_block_height).sum::<i32>()
        + view.blocks.len().saturating_sub(1) as i32 * 6
}

fn detail_actions_model(view: &DetailView) -> ModelRc<ContextActionRow> {
    let actions = view
        .actions
        .iter()
        .take(3)
        .map(|action| ContextActionRow {
            id: action.id.clone().into(),
            title: action.title.clone().into(),
            subtitle: String::new().into(),
            kind: "detail".into(),
            icon: slint::Image::default(),
            icon_kind: static_icon_kind(action.icon.as_ref()).into(),
            shortcut: String::new().into(),
        })
        .collect::<Vec<_>>();
    ModelRc::from(Rc::new(VecModel::from(actions)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailIconTarget {
    Block(usize),
    Action(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DetailMediaSource {
    Icon(IconSpec, u32),
    Image(std::path::PathBuf),
}

fn detail_icon_requests(view: &DetailView) -> Vec<(DetailIconTarget, DetailMediaSource)> {
    let blocks = view
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| match block {
            ResultDetailBlock::Icon {
                icon: Some(icon), ..
            } => Some((
                DetailIconTarget::Block(index),
                DetailMediaSource::Icon(icon.clone(), 40),
            )),
            ResultDetailBlock::Image { source, .. } => Some((
                DetailIconTarget::Block(index),
                DetailMediaSource::Image(source.into()),
            )),
            _ => None,
        });
    let actions = view
        .actions
        .iter()
        .take(3)
        .enumerate()
        .filter_map(|(index, action)| {
            let icon = action.icon.as_ref()?;
            static_icon_kind(Some(icon)).is_empty().then(|| {
                (
                    DetailIconTarget::Action(index),
                    DetailMediaSource::Icon(icon.clone(), 16),
                )
            })
        });
    blocks.chain(actions).collect()
}

fn request_detail_icons(
    window: &Launcher,
    view: &DetailView,
    service: &IconService,
    generation: i32,
) {
    let pending = detail_icon_requests(view);
    if pending.is_empty() {
        return;
    }
    let weak = window.as_weak();
    let service = service.clone();
    crate::utils::background::spawn(move || {
        let resolved = pending
            .into_iter()
            .filter_map(|(target, source)| {
                let image = match source {
                    DetailMediaSource::Icon(icon, size) => service.resolve_spec_at(&icon, size),
                    DetailMediaSource::Image(path) => service.resolve_detail_image(&path),
                };
                image.map(|image| (target, image))
            })
            .collect::<Vec<_>>();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = weak.upgrade() else {
                return;
            };
            if window.get_detail_content_generation() != generation {
                return;
            }
            for (target, icon) in resolved {
                match target {
                    DetailIconTarget::Block(index) => {
                        let model = window.get_detail_blocks();
                        if let Some(mut row) = model.row_data(index) {
                            row.icon = icon_to_image(icon.as_ref());
                            model.set_row_data(index, row);
                        }
                    }
                    DetailIconTarget::Action(index) => {
                        let model = window.get_detail_actions();
                        if let Some(mut row) = model.row_data(index) {
                            row.icon = icon_to_image(icon.as_ref());
                            model.set_row_data(index, row);
                        }
                    }
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::DetailAction;

    #[test]
    fn detail_models_defer_remote_icons_and_only_request_visible_actions() {
        let remote = IconSpec::Remote("https://example.invalid/icon.png".into());
        let action = |id: &str, icon| DetailAction {
            id: id.into(),
            title: id.into(),
            icon,
        };
        let view = DetailView {
            title: "Preview".into(),
            subtitle: String::new(),
            source: String::new(),
            blocks: vec![
                ResultDetailBlock::Separator,
                ResultDetailBlock::Icon {
                    glyph: String::new(),
                    color: String::new(),
                    icon: Some(remote.clone()),
                },
                ResultDetailBlock::Image {
                    source: "preview.png".into(),
                    alt: "Preview".into(),
                },
            ],
            actions: vec![
                action("static", Some(IconSpec::Calculator)),
                action("remote", Some(remote.clone())),
                action("plain", None),
                action("hidden", Some(remote.clone())),
            ],
            preferred_width: None,
            scrollable: false,
        };
        // Building models must not require networking or icon decoding.
        let blocks = detail_blocks_model(&view);
        let actions = detail_actions_model(&view);
        assert_eq!(blocks.row_count(), 3);
        assert_eq!(actions.row_count(), 3);
        assert_eq!(
            actions.row_data(0).unwrap().icon_kind.as_str(),
            "calculator"
        );
        assert_eq!(
            detail_icon_requests(&view),
            vec![
                (
                    DetailIconTarget::Block(1),
                    DetailMediaSource::Icon(remote.clone(), 40)
                ),
                (
                    DetailIconTarget::Block(2),
                    DetailMediaSource::Image("preview.png".into())
                ),
                (
                    DetailIconTarget::Action(1),
                    DetailMediaSource::Icon(remote, 16)
                ),
            ]
        );
    }
}
