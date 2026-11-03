use super::{SettingsCategory, SettingsEntry, SettingsShortcut, SettingsWindow};
use crate::{config, plugin::PluginSettingsDefinition, utils::i18n};
use anyhow::{anyhow, Context, Result};
use slint::{Model, ModelRc, VecModel};
use std::{path::PathBuf, rc::Rc, sync::OnceLock};

#[derive(Clone)]
struct Definition {
    id: &'static str,
    category: &'static str,
    title: &'static str,
    description: &'static str,
    kind: &'static str,
    options: Vec<&'static str>,
    minimum: i32,
    maximum: i32,
    step: i32,
}

fn definitions() -> Vec<Definition> {
    let mut languages = vec!["system"];
    languages.extend(i18n::SUPPORTED_LANGUAGES.iter().copied());
    let mut fields = vec![
        field(
            "hotkey",
            "general",
            "Global hotkey",
            "Click the field and press a modifier plus a key. Press Escape to cancel.",
            "hotkey",
        ),
        field(
            "show_on_startup",
            "general",
            "Show on startup",
            "Open the launcher when Wowie starts. Restart Wowie to apply.",
            "toggle",
        ),
        field(
            "hide_on_focus_loss",
            "general",
            "Hide when focus is lost",
            "Dismiss the launcher after another window receives focus.",
            "toggle",
        ),
        choice(
            "theme",
            "appearance",
            "Theme",
            "Choose the interface color theme.",
            &["system", "light", "dark"],
        ),
        choice(
            "language",
            "appearance",
            "Language",
            "Language used by Wowie and its built-in providers.",
            &languages,
        ),
        choice(
            "renderer",
            "appearance",
            "Renderer",
            "Graphics backend. Restart Wowie to switch the renderer.",
            &["femtovg", "femtovg-wgpu", "skia", "skia-opengl", "software"],
        ),
        number(
            "list_item_height",
            "appearance",
            "List row height",
            "Height of each result in list view.",
            36,
            96,
            2,
        ),
        choice(
            "font_family",
            "appearance",
            "Font family",
            "Choose an installed font family. Select System default to use the OS default.",
            &[],
        ),
        number(
            "font_size",
            "appearance",
            "Font size",
            "Base text size used throughout the interface.",
            11,
            20,
            1,
        ),
        field(
            "fuzzy_search",
            "search",
            "Fuzzy search",
            "Match results even when query characters are not adjacent.",
            "toggle",
        ),
        field(
            "compact_search",
            "search",
            "Compact empty search",
            "Hide results for an empty root search until you press Down.",
            "toggle",
        ),
        field(
            "separate_launch_entries",
            "search",
            "Separate launch entries",
            "Show each additional application launch action as its own result.",
            "toggle",
        ),
        field(
            "show_advanced_details",
            "search",
            "Advanced result details",
            "Show additional diagnostic details in result panels.",
            "toggle",
        ),
        field(
            "show_advanced_launch_entries",
            "search",
            "Advanced launch actions",
            "Include advanced launch actions in result menus.",
            "toggle",
        ),
        field(
            "providers.store_apps",
            "providers",
            "Store apps",
            "Include installed Microsoft Store apps.",
            "toggle",
        ),
        field(
            "providers.start_menu",
            "providers",
            "Start menu",
            "Include shortcuts from the Start menu.",
            "toggle",
        ),
        field(
            "providers.desktop",
            "providers",
            "Desktop shortcuts",
            "Include shortcuts from the desktop.",
            "toggle",
        ),
        field(
            "providers.control_panel",
            "providers",
            "Control Panel and Windows Settings",
            "Include Control Panel items and Windows Settings pages.",
            "toggle",
        ),
        field(
            "providers.indexed_extensions",
            "providers",
            "Indexed extensions",
            "Semicolon-separated file extensions, with or without dots.",
            "text",
        ),
        field(
            "shortcut_keywords.web_search",
            "shortcuts",
            "Web search keyword",
            "Keyword used to enter the built-in web search shortcut.",
            "text",
        ),
        field(
            "shortcut_keywords.command",
            "shortcuts",
            "Command keyword",
            "Keyword used to enter the built-in command shortcut.",
            "text",
        ),
    ];
    if cfg!(windows) {
        fields.insert(
            2,
            field(
                "run_at_sign_in",
                "general",
                "Run at sign-in",
                "Start Wowie when you sign in to Windows.",
                "toggle",
            ),
        );
    }
    fields
}

fn field(
    id: &'static str,
    category: &'static str,
    title: &'static str,
    description: &'static str,
    kind: &'static str,
) -> Definition {
    Definition {
        id,
        category,
        title,
        description,
        kind,
        options: Vec::new(),
        minimum: 0,
        maximum: 0,
        step: 1,
    }
}

fn choice(
    id: &'static str,
    category: &'static str,
    title: &'static str,
    description: &'static str,
    options: &[&'static str],
) -> Definition {
    Definition {
        options: options.to_vec(),
        ..field(id, category, title, description, "choice")
    }
}

fn number(
    id: &'static str,
    category: &'static str,
    title: &'static str,
    description: &'static str,
    minimum: i32,
    maximum: i32,
    step: i32,
) -> Definition {
    Definition {
        minimum,
        maximum,
        step,
        ..field(id, category, title, description, "number")
    }
}

pub(super) fn categories(
    language: &str,
    plugins: &[PluginSettingsDefinition],
) -> ModelRc<SettingsCategory> {
    let mut values = [
        ("general", "General"),
        ("appearance", "Appearance"),
        ("search", "Search"),
        ("providers", "Providers"),
        ("shortcuts", "Shortcuts"),
    ]
    .into_iter()
    .map(|(id, title)| SettingsCategory {
        id: id.into(),
        title: translate(language, title).into(),
        depth: 0,
        has_children: false,
    })
    .collect::<Vec<_>>();
    values.push(SettingsCategory {
        id: "plugins".into(),
        title: translate(language, "Plugins").into(),
        depth: 0,
        has_children: !plugins.is_empty(),
    });
    for plugin in plugins {
        values.push(SettingsCategory {
            id: plugin_category(&plugin.id).into(),
            title: i18n::tr_owned_in_context(language, "Settings", &plugin.name).into(),
            depth: 1,
            has_children: false,
        });
    }
    ModelRc::from(Rc::new(VecModel::from(values)))
}

pub(super) fn refresh(
    window: &SettingsWindow,
    config: &config::Config,
    plugins: &[PluginSettingsDefinition],
    language: &str,
) {
    let previous_category = window.get_selected_category().to_string();
    let selected = previous_category.clone();
    let selected = if selected.starts_with("plugin/")
        && !plugins
            .iter()
            .any(|plugin| plugin_category(&plugin.id) == selected)
    {
        "general".to_owned()
    } else {
        selected
    };
    window.set_categories(categories(language, plugins));
    window.set_selected_category(selected.clone().into());
    if selected.starts_with("plugin/") {
        window.set_plugins_expanded(true);
    }
    window.set_selected_category_title(category_title(&selected, language, plugins).into());
    if selected != previous_category {
        window.set_settings_scroll_y(0.0);
    }
    update_entries(window, entries(config, plugins, &selected, language));
}

pub(super) fn update_entries(window: &SettingsWindow, entries: ModelRc<SettingsEntry>) {
    let current = window.get_entries();
    let Some(model) = current.as_any().downcast_ref::<VecModel<SettingsEntry>>() else {
        window.set_entries(entries);
        return;
    };

    let next = entries.iter().collect::<Vec<_>>();
    let shared_count = model.row_count().min(next.len());
    for (index, entry) in next.iter().take(shared_count).enumerate() {
        if !model
            .row_data(index)
            .is_some_and(|current| entries_match(&current, entry))
        {
            model.set_row_data(index, entry.clone());
        }
    }
    while model.row_count() > next.len() {
        model.remove(model.row_count() - 1);
    }
    if next.len() > shared_count {
        model.extend(next.into_iter().skip(shared_count));
    }
}

fn entries_match(left: &SettingsEntry, right: &SettingsEntry) -> bool {
    left.id == right.id
        && left.category == right.category
        && left.title == right.title
        && left.description == right.description
        && left.value == right.value
        && left.kind == right.kind
        && left.choice_labels.iter().eq(right.choice_labels.iter())
        && left.choice_values.iter().eq(right.choice_values.iter())
        && left.choice_index == right.choice_index
        && left.minimum == right.minimum
        && left.maximum == right.maximum
        && left.step == right.step
        && left.shortcut.name == right.shortcut.name
        && left.shortcut.prefix == right.shortcut.prefix
        && left.shortcut.placeholder == right.shortcut.placeholder
        && left.shortcut.shortcut_type == right.shortcut.shortcut_type
        && left.shortcut.mode == right.shortcut.mode
        && left.shortcut.template == right.shortcut.template
        && left.shortcut.target == right.shortcut.target
        && left.shortcut.arguments == right.shortcut.arguments
        && left.shortcut.description == right.shortcut.description
}

pub(super) fn category_title(
    category: &str,
    language: &str,
    plugins: &[PluginSettingsDefinition],
) -> String {
    plugins
        .iter()
        .find(|plugin| plugin_category(&plugin.id) == category)
        .map(|plugin| i18n::tr_owned_in_context(language, "Settings", &plugin.name))
        .or_else(|| {
            [
                ("general", "General"),
                ("appearance", "Appearance"),
                ("search", "Search"),
                ("providers", "Providers"),
                ("shortcuts", "Shortcuts"),
                ("plugins", "Plugins"),
            ]
            .into_iter()
            .find(|(id, _)| *id == category)
            .map(|(_, title)| translate(language, title).to_owned())
        })
        .unwrap_or_else(|| category.to_owned())
}

pub(super) fn entries(
    config: &config::Config,
    plugins: &[PluginSettingsDefinition],
    category: &str,
    language: &str,
) -> ModelRc<SettingsEntry> {
    let mut values = definitions()
        .into_iter()
        .filter(|field| field.category == category)
        .map(|field| entry_from_definition(config, category, language, field))
        .collect::<Vec<_>>();
    if category == "providers" {
        values.push(plugin_entry(
            category,
            "providers.indexed_directories:add".into(),
            translate(language, "Add indexed folder"),
            translate(
                language,
                "Choose an existing folder, or paste its path below.",
            ),
            "directory-add",
            String::new(),
            0,
            0,
            0,
            0,
        ));
        for (index, path) in config.providers.indexed_directories.iter().enumerate() {
            values.push(plugin_entry(
                category,
                format!("providers.indexed_directories:{index}"),
                translate(language, "Indexed folder"),
                String::new(),
                "directory",
                path.to_string_lossy().into_owned(),
                0,
                0,
                0,
                0,
            ));
        }
    }
    if category == "shortcuts" {
        let mut add = plugin_entry(
            category,
            "shortcuts:add".into(),
            translate(language, "Add shortcut"),
            translate(language, "Create a shortcut with separate fields."),
            "shortcut-add",
            String::new(),
            0,
            0,
            0,
            0,
        );
        add.shortcut = SettingsShortcut {
            shortcut_type: "url".into(),
            mode: "execute".into(),
            ..Default::default()
        };
        values.push(add);
        for (index, shortcut) in config.shortcuts.iter().enumerate() {
            let mut entry = plugin_entry(
                category,
                format!("shortcuts:{index}"),
                shortcut.name.clone(),
                format!("{} · {}", shortcut.prefix.trim(), shortcut.description),
                "shortcut",
                String::new(),
                0,
                0,
                0,
                0,
            );
            entry.choice_index = index as i32;
            entry.shortcut = SettingsShortcut {
                name: shortcut.name.clone().into(),
                prefix: shortcut.prefix.trim().into(),
                placeholder: shortcut.placeholder.clone().into(),
                shortcut_type: shortcut.shortcut_type.clone().into(),
                mode: shortcut.mode.clone().into(),
                template: shortcut.template.clone().into(),
                target: shortcut.target.clone().into(),
                arguments: shortcut.arguments.clone().into(),
                description: shortcut.description.clone().into(),
            };
            values.push(entry);
        }
    }
    if category == "search" {
        values.push(plugin_entry(
            category,
            "priority:category-heading".into(),
            translate(language, "Result type order"),
            translate(
                language,
                "Favorites and session history stay first. Among other results, stronger text matches stay first; this order breaks ties.",
            ),
            "priority-heading",
            String::new(),
            0,
            0,
            0,
            0,
        ));
        let result_names = [
            ("application", "Applications"),
            ("game", "Games"),
            ("website", "Websites"),
            ("calculator", "Calculator"),
            ("control_panel", "Control Panel results"),
            ("settings", "Settings results"),
            ("document", "Documents"),
            ("other", "Other results"),
            ("unclassified", "Unclassified results"),
        ]
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();
        for (index, key) in config.result_order.iter().enumerate() {
            let title = result_names
                .get(key.as_str())
                .copied()
                .unwrap_or("Other results");
            values.push(priority_entry(
                category,
                format!("priority:category:{key}"),
                translate(language, title),
                translate(language, "Move this result type earlier or later."),
                index,
                config.result_order.len(),
            ));
        }

        values.push(plugin_entry(
            category,
            "priority:plugin-heading".into(),
            translate(language, "Plugin order"),
            translate(
                language,
                "Within the same result type, this order breaks ties between plugins.",
            ),
            "priority-heading",
            String::new(),
            0,
            0,
            0,
            0,
        ));
        let plugin_order = visible_plugin_order(config, plugins);
        for (index, plugin_id) in plugin_order.iter().enumerate() {
            if let Some(plugin) = plugins.iter().find(|plugin| plugin.id == *plugin_id) {
                values.push(priority_entry(
                    category,
                    format!("priority:plugin:{}", plugin.id),
                    i18n::tr_owned_in_context(language, "Settings", &plugin.name),
                    translate(language, "Move this plugin earlier or later."),
                    index,
                    plugin_order.len(),
                ));
            }
        }
        values.push(plugin_entry(
            category,
            "priority:reset".into(),
            translate(language, "Reset result order"),
            translate(
                language,
                "Restore the default order for result types and plugins.",
            ),
            "priority-reset",
            String::new(),
            0,
            0,
            0,
            0,
        ));
    }
    if let Some(plugin) = plugins
        .iter()
        .find(|plugin| plugin_category(&plugin.id) == category)
    {
        let plugin_config = config.plugins.get(&plugin.id).cloned().unwrap_or_default();
        values.push(plugin_entry(
            category,
            format!("plugin:{}:enabled", plugin.id),
            translate(language, "Enabled"),
            translate(language, "Enable or disable this plugin."),
            "toggle",
            plugin_config.enabled.to_string(),
            0,
            0,
            0,
            1,
        ));
        for preference in &plugin.preferences {
            let kind = match preference.value_type.as_str() {
                "password" => "secret",
                "boolean" | "bool" => "toggle",
                "number" | "integer" => "number",
                "enum" if !preference.options.is_empty() => "choice",
                _ => "text",
            };
            let value = plugin_config
                .preferences
                .get(&preference.key)
                .unwrap_or(&preference.default);
            let value_text = match value {
                serde_json::Value::String(value) => value.clone(),
                serde_json::Value::Null if kind == "number" => "0".into(),
                serde_json::Value::Null => String::new(),
                _ => value.to_string(),
            };
            let choice_index = preference
                .options
                .iter()
                .position(|option| option == &value_text)
                .unwrap_or(0) as i32;
            let mut entry = plugin_entry(
                category,
                format!("plugin:{}:pref:{}", plugin.id, preference.key),
                i18n::tr_owned_in_context(language, "Settings", &preference.title),
                i18n::tr_owned_in_context(language, "Settings", &preference.description),
                kind,
                value_text,
                value.as_i64().unwrap_or(0) as i32,
                -1000,
                1000,
                10,
            );
            if kind == "choice" {
                entry.choice_values = choice_model(preference.options.clone());
                let labels = if preference.option_labels.len() == preference.options.len() {
                    preference
                        .option_labels
                        .iter()
                        .map(|label| i18n::tr_owned_in_context(language, "Settings", label))
                        .collect()
                } else {
                    preference.options.clone()
                };
                entry.choice_labels = choice_model(labels);
                entry.choice_index = choice_index;
            }
            values.push(entry);
        }
        for command in plugin
            .commands
            .iter()
            .filter(|command| command.customizable_action_words)
        {
            let words = plugin_config
                .command_action_words
                .get(&command.id)
                .unwrap_or(&command.action_words)
                .join(", ");
            let command_title = if command.title.trim().is_empty() {
                command.id.as_str()
            } else {
                command.title.as_str()
            };
            values.push(plugin_entry(
                category,
                format!("plugin:{}:command:{}:action_words", plugin.id, command.id),
                format!("{}: {command_title}", translate(language, "Trigger words")),
                translate(
                    language,
                    "Comma-separated words that activate this command. Leave empty to use plugin defaults.",
                ),
                "text",
                words,
                0,
                0,
                0,
                0,
            ));
        }
    } else if category == "plugins" {
        let (title, description) = if plugins.is_empty() {
            (
                translate(language, "No plugins installed"),
                translate(language, "Install a plugin to manage its settings here."),
            )
        } else {
            (
                translate(language, "Plugin settings"),
                translate(
                    language,
                    "Select a plugin from the sidebar to manage its settings.",
                ),
            )
        };
        values.push(plugin_entry(
            category,
            "plugins.info".into(),
            title,
            description,
            "info",
            String::new(),
            0,
            0,
            0,
            0,
        ));
    }
    ModelRc::from(Rc::new(VecModel::from(values)))
}

fn entry_from_definition(
    config: &config::Config,
    category: &str,
    language: &str,
    field: Definition,
) -> SettingsEntry {
    let value = value_for(config, field.id);
    let mut options = field
        .options
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    if field.id == "font_family" {
        options = font_family_options(&value);
    }
    let labels = options
        .iter()
        .map(|value| choice_label(language, field.id, value))
        .collect::<Vec<_>>();
    let choice_index = options
        .iter()
        .position(|option| option == &value)
        .map(|index| index as i32)
        .unwrap_or(-1);
    SettingsEntry {
        id: field.id.into(),
        category: category.into(),
        title: translate(language, field.title).into(),
        description: translate(language, field.description).into(),
        value: value.into(),
        kind: field.kind.into(),
        choice_labels: choice_model(labels),
        choice_values: choice_model(options),
        choice_index,
        minimum: field.minimum,
        maximum: field.maximum,
        step: field.step,
        shortcut: SettingsShortcut::default(),
        drag_data: slint::DataTransfer::default(),
    }
}

#[allow(clippy::too_many_arguments)]
fn plugin_entry<T: Into<String>, D: Into<String>>(
    category: &str,
    id: String,
    title: T,
    description: D,
    kind: &str,
    value: String,
    _number_value: i32,
    minimum: i32,
    maximum: i32,
    step: i32,
) -> SettingsEntry {
    SettingsEntry {
        id: id.into(),
        category: category.into(),
        title: slint::SharedString::from(title.into()),
        description: slint::SharedString::from(description.into()),
        value: value.into(),
        kind: kind.into(),
        choice_labels: choice_model(Vec::new()),
        choice_values: choice_model(Vec::new()),
        choice_index: -1,
        minimum,
        maximum,
        step,
        shortcut: SettingsShortcut::default(),
        drag_data: slint::DataTransfer::default(),
    }
}

fn priority_entry(
    category: &str,
    id: String,
    title: impl Into<String>,
    description: impl Into<String>,
    index: usize,
    count: usize,
) -> SettingsEntry {
    let rank = index + 1;
    let drag_data = slint::DataTransfer::from(slint::SharedString::from(id.clone()));
    let mut entry = plugin_entry(
        category,
        id,
        title,
        description,
        "priority",
        rank.to_string(),
        rank as i32,
        0,
        count.saturating_sub(1) as i32,
        0,
    );
    entry.choice_index = index as i32;
    entry.drag_data = drag_data;
    entry
}

pub(super) fn visible_plugin_order(
    config: &config::Config,
    plugins: &[PluginSettingsDefinition],
) -> Vec<String> {
    let mut available = plugins
        .iter()
        .map(|plugin| plugin.id.clone())
        .collect::<Vec<_>>();
    available.sort();
    let mut order = config
        .plugin_order
        .iter()
        .filter(|id| available.contains(id))
        .cloned()
        .collect::<Vec<_>>();
    for id in available {
        if !order.contains(&id) {
            order.push(id);
        }
    }
    order
}

fn choice_model(values: Vec<String>) -> ModelRc<slint::SharedString> {
    let values = values
        .into_iter()
        .map(slint::SharedString::from)
        .collect::<Vec<_>>();
    ModelRc::from(Rc::new(VecModel::from(values)))
}

fn installed_font_families() -> &'static [String] {
    // UI reads must never initialize or wait for the font database.
    FONT_FAMILIES.get().map(Vec::as_slice).unwrap_or_default()
}

static FONT_FAMILIES: OnceLock<Vec<String>> = OnceLock::new();

pub(super) fn preload_font_families(window: slint::Weak<SettingsWindow>) {
    std::thread::spawn(move || {
        FONT_FAMILIES.get_or_init(scan_font_families);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            if window.get_selected_category() != "appearance" {
                return;
            }
            // Update only the font row from its current value, so a late scan
            // cannot overwrite edits or replace a different settings page.
            let entries = window.get_entries();
            let Some(model) = entries.as_any().downcast_ref::<VecModel<SettingsEntry>>() else {
                return;
            };
            let (config, _) = config::Config::load();
            for index in 0..model.row_count() {
                if let Some(mut entry) = model.row_data(index) {
                    if entry.id == "font_family" {
                        let options = font_family_options(entry.value.as_str());
                        entry.choice_index = options
                            .iter()
                            .position(|option| option == entry.value.as_str())
                            .map(|index| index as i32)
                            .unwrap_or(-1);
                        entry.choice_labels = choice_model(
                            options
                                .iter()
                                .map(|value| choice_label(&config.language, "font_family", value))
                                .collect(),
                        );
                        entry.choice_values = choice_model(options);
                        model.set_row_data(index, entry);
                        break;
                    }
                }
            }
        });
    });
}

fn font_family_options(value: &str) -> Vec<String> {
    let mut options = vec![String::new()];
    options.extend(installed_font_families().iter().cloned());
    if !value.is_empty()
        && !options
            .iter()
            .any(|option| option.eq_ignore_ascii_case(value))
    {
        options.push(value.to_owned());
    }
    options
}

fn scan_font_families() -> Vec<String> {
    let mut database = fontdb::Database::new();
    database.load_system_fonts();

    let mut families = database
        .faces()
        .filter_map(|face| {
            face.families
                .first()
                .map(|(family, _)| family.trim().to_owned())
        })
        .filter(|family| !family.is_empty())
        .collect::<Vec<_>>();
    families.sort_unstable_by_key(|family| family.to_lowercase());
    families.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    families
}

fn choice_label(language: &str, setting_id: &str, value: &str) -> String {
    let message = match (setting_id, value) {
        ("theme", "system") => Some("System"),
        ("theme", "light") => Some("Light"),
        ("theme", "dark") => Some("Dark"),
        ("language", "system") => Some("System default"),
        ("font_family", "") => Some("System default"),
        ("language", "en") => Some("English"),
        ("language", "zh-CN") => Some("Simplified Chinese"),
        _ => None,
    };
    message
        .map(|message| translate(language, message))
        .unwrap_or_else(|| match value {
            "femtovg" => "FemtoVG".into(),
            "femtovg-wgpu" => "FemtoVG (WGPU)".into(),
            "skia-opengl" => "Skia OpenGL".into(),
            value => value.to_owned(),
        })
}

fn value_for(config: &config::Config, id: &str) -> String {
    match id {
        "providers.indexed_extensions" => config.providers.indexed_extensions.join(";"),
        _ if id.starts_with("plugin:") => {
            let rest = id.trim_start_matches("plugin:");
            let Some((plugin_id, field)) = rest.split_once(':') else {
                return String::new();
            };
            let plugin = config.plugins.get(plugin_id).cloned().unwrap_or_default();
            match field {
                "enabled" => plugin.enabled.to_string(),
                field if field.starts_with("pref:") => plugin
                    .preferences
                    .get(field.trim_start_matches("pref:"))
                    .map(|value| match value {
                        serde_json::Value::String(value) => value.clone(),
                        serde_json::Value::Null => String::new(),
                        _ => value.to_string(),
                    })
                    .unwrap_or_default(),
                _ => String::new(),
            }
        }
        _ => serde_json::to_value(config)
            .ok()
            .and_then(|value| {
                value
                    .pointer(&format!("/{}", id.replace('.', "/")))
                    .cloned()
            })
            .map(|value| match value {
                serde_json::Value::String(value) => value,
                _ => value.to_string(),
            })
            .unwrap_or_default(),
    }
}

pub(super) fn apply(
    config: &mut config::Config,
    plugins: &[PluginSettingsDefinition],
    id: &str,
    submitted: &str,
) -> Result<()> {
    if id == "plugins.info" {
        return Ok(());
    }
    if id == "priority:reset" {
        config.result_order = config::default_result_order();
        config.plugin_order = plugins.iter().map(|plugin| plugin.id.clone()).collect();
        config.plugin_order.sort();
        return Ok(());
    }
    if let Some(category_id) = id.strip_prefix("priority:category:") {
        if !config::default_result_order()
            .iter()
            .any(|known| known == category_id)
        {
            return Err(anyhow!("unknown result category: {category_id}"));
        }
        move_order(&mut config.result_order, category_id, submitted)?;
        return Ok(());
    }
    if let Some(plugin_id) = id.strip_prefix("priority:plugin:") {
        if !plugins.iter().any(|plugin| plugin.id == plugin_id) {
            return Err(anyhow!("plugin is not installed: {plugin_id}"));
        }
        let mut order = visible_plugin_order(config, plugins);
        order.extend(
            config
                .plugin_order
                .iter()
                .filter(|id| !plugins.iter().any(|plugin| plugin.id == id.as_str()))
                .cloned(),
        );
        move_order(&mut order, plugin_id, submitted)?;
        config.plugin_order = order;
        return Ok(());
    }
    if let Some(index) = id.strip_prefix("providers.indexed_directories:") {
        if submitted == "__remove__" && index != "add" {
            let directories = &mut config.providers.indexed_directories;
            let index = index.parse::<usize>().context("invalid indexed folder")?;
            if index >= directories.len() {
                return Err(anyhow!("indexed folder no longer exists"));
            }
            directories.remove(index);
        } else {
            set_directory(config, id, PathBuf::from(submitted.trim()))?;
        }
        return Ok(());
    }
    let value = match submitted {
        "__cycle__" => {
            let field = definitions()
                .into_iter()
                .find(|field| field.id == id)
                .ok_or_else(|| anyhow!("setting does not support choices: {id}"))?;
            let current = value_for(config, id);
            let index = field
                .options
                .iter()
                .position(|option| *option == current)
                .unwrap_or(0);
            field.options[(index + 1) % field.options.len()].to_owned()
        }
        "__increment__" | "__decrement__" => {
            let current = value_for(config, id)
                .parse::<i32>()
                .ok()
                .or_else(|| plugin_preference_default(config, plugins, id))
                .ok_or_else(|| anyhow!("setting is not numeric: {id}"))?;
            let (step, minimum, maximum) = numeric_bounds(id, plugins)?;
            let delta = if submitted == "__increment__" {
                step
            } else {
                -step
            };
            current
                .saturating_add(delta)
                .clamp(minimum, maximum)
                .to_string()
        }
        _ => submitted.to_owned(),
    };
    match id {
        "hotkey" => config.hotkey = value,
        "show_on_startup" => config.show_on_startup = parse_bool(&value)?,
        "run_at_sign_in" => config.run_at_sign_in = parse_bool(&value)?,
        "hide_on_focus_loss" => config.hide_on_focus_loss = parse_bool(&value)?,
        "theme" => config.theme = value,
        "language" => config.language = value,
        "renderer" => config.renderer = value,
        "list_item_height" => config.list_item_height = parse_u32(&value, id)?,
        "font_family" => config.font_family = value.trim().to_owned(),
        "font_size" => config.font_size = parse_u32(&value, id)?.clamp(11, 20),
        "shortcut_keywords.web_search" => {
            set_builtin_shortcut_keyword(config, plugins, "web_search", &value)?
        }
        "shortcut_keywords.command" => {
            set_builtin_shortcut_keyword(config, plugins, "command", &value)?
        }
        "fuzzy_search" => config.fuzzy_search = parse_bool(&value)?,
        "compact_search" => config.compact_search = parse_bool(&value)?,
        "separate_launch_entries" => config.separate_launch_entries = parse_bool(&value)?,
        "show_advanced_details" => config.show_advanced_details = parse_bool(&value)?,
        "show_advanced_launch_entries" => config.show_advanced_launch_entries = parse_bool(&value)?,
        "providers.store_apps" => config.providers.store_apps = parse_bool(&value)?,
        "providers.start_menu" => config.providers.start_menu = parse_bool(&value)?,
        "providers.desktop" => config.providers.desktop = parse_bool(&value)?,
        "providers.control_panel" => config.providers.control_panel = parse_bool(&value)?,
        "providers.indexed_extensions" => {
            config.providers.indexed_extensions = value
                .split(';')
                .map(|extension| {
                    extension
                        .trim()
                        .trim_start_matches('.')
                        .to_ascii_lowercase()
                })
                .filter(|extension| !extension.is_empty())
                .collect();
        }
        _ if id.starts_with("plugin:") => apply_plugin_setting(config, plugins, id, &value)?,
        _ => return Err(anyhow!("unknown setting: {id}")),
    }
    Ok(())
}

pub(super) fn set_directory(config: &mut config::Config, id: &str, path: PathBuf) -> Result<()> {
    if !path.is_dir() {
        return Err(anyhow!("folder does not exist: {}", path.display()));
    }
    let index = id
        .strip_prefix("providers.indexed_directories:")
        .ok_or_else(|| anyhow!("invalid indexed folder setting"))?;
    let directories = &mut config.providers.indexed_directories;
    if index == "add" {
        if directories
            .iter()
            .any(|existing| same_folder(existing, &path))
        {
            return Err(anyhow!("folder is already indexed"));
        }
        directories.push(path);
    } else {
        let index = index.parse::<usize>().context("invalid indexed folder")?;
        if index >= directories.len() {
            return Err(anyhow!("indexed folder no longer exists"));
        }
        if directories
            .iter()
            .enumerate()
            .any(|(other, existing)| other != index && same_folder(existing, &path))
        {
            return Err(anyhow!("folder is already indexed"));
        }
        directories[index] = path;
    }
    Ok(())
}

pub(super) fn save_shortcut(
    config: &mut config::Config,
    index: i32,
    value: SettingsShortcut,
    plugins: &[PluginSettingsDefinition],
) -> Result<()> {
    let shortcut = config::ShortcutConfig {
        name: value.name.trim().to_owned(),
        prefix: value.prefix.trim().to_owned(),
        placeholder: value.placeholder.trim().to_owned(),
        shortcut_type: value.shortcut_type.to_string(),
        mode: value.mode.to_string(),
        template: value.template.trim().to_owned(),
        target: value.target.trim().to_owned(),
        arguments: value.arguments.trim().to_owned(),
        description: value.description.trim().to_owned(),
    };
    if shortcut.name.is_empty() || shortcut.prefix.is_empty() {
        return Err(anyhow!("shortcut name and prefix are required"));
    }
    if !matches!(
        shortcut.shortcut_type.as_str(),
        "url" | "application" | "command"
    ) {
        return Err(anyhow!("invalid shortcut type"));
    }
    if !matches!(shortcut.mode.as_str(), "execute" | "search" | "panel") {
        return Err(anyhow!("invalid shortcut mode"));
    }
    if shortcut.shortcut_type == "url" && !shortcut.template.contains("{query}") {
        return Err(anyhow!("URL template must contain {{query}}"));
    }
    if shortcut.shortcut_type != "url" && shortcut.target.is_empty() {
        return Err(anyhow!("application or command target is required"));
    }
    let keyword = shortcut.prefix.trim_end();
    if config
        .shortcut_keywords
        .web_search
        .eq_ignore_ascii_case(keyword)
        || config
            .shortcut_keywords
            .command
            .eq_ignore_ascii_case(keyword)
    {
        return Err(anyhow!(
            "shortcut prefix is already used by a built-in shortcut"
        ));
    }
    if plugins.iter().any(|plugin| {
        plugin.commands.iter().any(|command| {
            config
                .plugins
                .get(&plugin.id)
                .and_then(|settings| settings.command_action_words.get(&command.id))
                .unwrap_or(&command.action_words)
                .iter()
                .any(|word| word.eq_ignore_ascii_case(keyword))
        })
    }) {
        return Err(anyhow!("shortcut prefix is already used by a plugin"));
    }
    if config
        .shortcuts
        .iter()
        .enumerate()
        .any(|(other, existing)| other as i32 != index && existing.prefix.trim() == shortcut.prefix)
    {
        return Err(anyhow!("shortcut prefix is already in use"));
    }
    if index == -1 {
        config.shortcuts.push(shortcut);
    } else {
        let existing = config
            .shortcuts
            .get_mut(index as usize)
            .ok_or_else(|| anyhow!("shortcut no longer exists"))?;
        *existing = shortcut;
    }
    Ok(())
}

pub(super) fn remove_shortcut(config: &mut config::Config, index: i32) -> Result<()> {
    if index < 0 || index as usize >= config.shortcuts.len() {
        return Err(anyhow!("shortcut no longer exists"));
    }
    config.shortcuts.remove(index as usize);
    Ok(())
}

fn same_folder(left: &std::path::Path, right: &std::path::Path) -> bool {
    left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase()
}

fn move_order(order: &mut [String], id: &str, direction: &str) -> Result<()> {
    let index = order
        .iter()
        .position(|item| item == id)
        .ok_or_else(|| anyhow!("priority item no longer exists: {id}"))?;
    match direction {
        "__move_up__" if index > 0 => order.swap(index, index - 1),
        "__move_down__" if index + 1 < order.len() => order.swap(index, index + 1),
        "__move_up__" | "__move_down__" => {}
        _ => return Err(anyhow!("invalid priority move: {direction}")),
    }
    Ok(())
}

pub(super) fn can_move_priority(source: &str, target: &str) -> bool {
    (source.starts_with("priority:category:") && target.starts_with("priority:category:"))
        || (source.starts_with("priority:plugin:") && target.starts_with("priority:plugin:"))
}

pub(super) fn move_priority(
    config: &mut config::Config,
    plugins: &[PluginSettingsDefinition],
    source: &str,
    target: &str,
    after: bool,
) -> Result<()> {
    if let (Some(source_id), Some(target_id)) = (
        source.strip_prefix("priority:category:"),
        target.strip_prefix("priority:category:"),
    ) {
        return move_relative(&mut config.result_order, source_id, target_id, after);
    }
    if let (Some(source_id), Some(target_id)) = (
        source.strip_prefix("priority:plugin:"),
        target.strip_prefix("priority:plugin:"),
    ) {
        let mut order = visible_plugin_order(config, plugins);
        order.extend(
            config
                .plugin_order
                .iter()
                .filter(|id| !plugins.iter().any(|plugin| plugin.id == id.as_str()))
                .cloned(),
        );
        move_relative(&mut order, source_id, target_id, after)?;
        config.plugin_order = order;
        return Ok(());
    }
    Err(anyhow!(
        "priority items must be dropped within the same list"
    ))
}

fn move_relative(order: &mut Vec<String>, source: &str, target: &str, after: bool) -> Result<()> {
    if source == target {
        return Ok(());
    }
    let source_index = order
        .iter()
        .position(|item| item == source)
        .ok_or_else(|| anyhow!("priority item no longer exists: {source}"))?;
    if !order.iter().any(|item| item == target) {
        return Err(anyhow!("priority drop target no longer exists: {target}"));
    }
    let item = order.remove(source_index);
    let target_index = order
        .iter()
        .position(|item| item == target)
        .ok_or_else(|| anyhow!("priority drop target no longer exists: {target}"))?;
    order.insert(target_index + usize::from(after), item);
    Ok(())
}

fn plugin_preference_default<'a>(
    config: &'a config::Config,
    plugins: &'a [PluginSettingsDefinition],
    id: &str,
) -> Option<i32> {
    let rest = id.strip_prefix("plugin:")?;
    let (plugin_id, field) = rest.split_once(":pref:")?;
    if config
        .plugins
        .get(plugin_id)
        .is_some_and(|plugin| plugin.preferences.contains_key(field))
    {
        return None;
    }
    plugins
        .iter()
        .find(|plugin| plugin.id == plugin_id)?
        .preferences
        .iter()
        .find(|preference| preference.key == field)
        .map(|preference| preference.default.as_i64().unwrap_or(0) as i32)
}

fn numeric_bounds(id: &str, plugins: &[PluginSettingsDefinition]) -> Result<(i32, i32, i32)> {
    if let Some(field) = definitions().into_iter().find(|field| field.id == id) {
        return Ok((field.step, field.minimum, field.maximum));
    }
    if id.starts_with("plugin:") {
        if let Some((plugin_id, key)) = id.trim_start_matches("plugin:").split_once(":pref:") {
            let preference = plugins
                .iter()
                .find(|plugin| plugin.id == plugin_id)
                .and_then(|plugin| plugin.preferences.iter().find(|item| item.key == key));
            if preference.is_some_and(|preference| {
                matches!(preference.value_type.as_str(), "number" | "integer")
            }) {
                return Ok((10, -1000, 1000));
            }
        }
    }
    Err(anyhow!("setting does not support numeric input: {id}"))
}

fn apply_plugin_setting(
    config: &mut config::Config,
    plugins: &[PluginSettingsDefinition],
    id: &str,
    value: &str,
) -> Result<()> {
    let rest = id.strip_prefix("plugin:").unwrap_or_default();
    let (plugin_id, field) = rest
        .split_once(':')
        .ok_or_else(|| anyhow!("invalid plugin setting id: {id}"))?;
    let definition = plugins
        .iter()
        .find(|plugin| plugin.id == plugin_id)
        .ok_or_else(|| anyhow!("plugin is not installed: {plugin_id}"))?;
    let command_override = field
        .strip_prefix("command:")
        .and_then(|field| field.strip_suffix(":action_words"))
        .map(|command_id| {
            let command = definition
                .commands
                .iter()
                .find(|command| command.id == command_id)
                .filter(|command| command.customizable_action_words)
                .ok_or_else(|| anyhow!("plugin command does not support custom trigger words"))?;
            let words = parse_action_words(value)?;
            validate_plugin_action_words(config, plugins, plugin_id, &command.id, &words)?;
            Ok::<_, anyhow::Error>((command.id.clone(), words))
        })
        .transpose()?;
    let plugin = config.plugins.entry(plugin_id.into()).or_default();
    match field {
        "enabled" => plugin.enabled = parse_bool(value)?,
        field if field.starts_with("pref:") => {
            let key = field.trim_start_matches("pref:");
            let preference = definition
                .preferences
                .iter()
                .find(|preference| preference.key == key)
                .ok_or_else(|| anyhow!("plugin preference is not declared: {key}"))?;
            let json_value = match preference.value_type.as_str() {
                "boolean" | "bool" => serde_json::Value::Bool(parse_bool(value)?),
                "number" | "integer" => serde_json::Value::Number(parse_i32(value, id)?.into()),
                "enum" if preference.options.iter().any(|option| option == value) => {
                    serde_json::Value::String(value.to_owned())
                }
                "enum" => return Err(anyhow!("invalid option for plugin setting: {key}")),
                _ => serde_json::Value::String(value.to_owned()),
            };
            plugin.preferences.insert(key.to_owned(), json_value);
        }
        _ if command_override.is_some() => {
            let (command_id, words) = command_override.expect("checked above");
            if words.is_empty() {
                plugin.command_action_words.remove(&command_id);
            } else {
                plugin.command_action_words.insert(command_id, words);
            }
        }
        _ => return Err(anyhow!("unknown plugin setting: {field}")),
    }
    Ok(())
}

fn set_builtin_shortcut_keyword(
    config: &mut config::Config,
    plugins: &[PluginSettingsDefinition],
    id: &str,
    submitted: &str,
) -> Result<()> {
    let keyword = submitted.trim();
    if keyword.is_empty() || keyword.chars().any(char::is_whitespace) {
        return Err(anyhow!("shortcut keyword must be one non-empty word"));
    }
    let other_builtin = if id == "web_search" {
        &config.shortcut_keywords.command
    } else {
        &config.shortcut_keywords.web_search
    };
    if keyword.eq_ignore_ascii_case(other_builtin) {
        return Err(anyhow!("built-in shortcut keywords must be different"));
    }
    if config
        .shortcuts
        .iter()
        .any(|shortcut| shortcut.prefix.trim_end().eq_ignore_ascii_case(keyword))
    {
        return Err(anyhow!("shortcut keyword is already in use"));
    }
    if plugins.iter().any(|plugin| {
        plugin.commands.iter().any(|command| {
            config
                .plugins
                .get(&plugin.id)
                .and_then(|settings| settings.command_action_words.get(&command.id))
                .unwrap_or(&command.action_words)
                .iter()
                .any(|word| word.eq_ignore_ascii_case(keyword))
        })
    }) {
        return Err(anyhow!("shortcut keyword is already in use by a plugin"));
    }
    match id {
        "web_search" => config.shortcut_keywords.web_search = keyword.to_owned(),
        "command" => config.shortcut_keywords.command = keyword.to_owned(),
        _ => return Err(anyhow!("unknown built-in shortcut keyword: {id}")),
    }
    Ok(())
}

fn parse_action_words(value: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    for word in value.split([',', ';', '\n', '\r']) {
        let word = word.trim();
        if word.is_empty() {
            continue;
        }
        if word.chars().any(char::is_whitespace) {
            return Err(anyhow!("each trigger word must be a single word"));
        }
        if word.chars().count() > 48 {
            return Err(anyhow!("trigger words must be 48 characters or fewer"));
        }
        if words
            .iter()
            .any(|known: &String| known.eq_ignore_ascii_case(word))
        {
            continue;
        }
        words.push(word.to_owned());
        if words.len() > 16 {
            return Err(anyhow!("a command can have at most 16 trigger words"));
        }
    }
    Ok(words)
}

fn validate_plugin_action_words(
    config: &config::Config,
    plugins: &[PluginSettingsDefinition],
    plugin_id: &str,
    command_id: &str,
    words: &[String],
) -> Result<()> {
    let collides = |candidate: &str| {
        config
            .shortcut_keywords
            .web_search
            .eq_ignore_ascii_case(candidate)
            || config
                .shortcut_keywords
                .command
                .eq_ignore_ascii_case(candidate)
            || config
                .shortcuts
                .iter()
                .any(|shortcut| shortcut.prefix.trim_end().eq_ignore_ascii_case(candidate))
            || plugins.iter().any(|plugin| {
                plugin.commands.iter().any(|command| {
                    if plugin.id == plugin_id && command.id == command_id {
                        return false;
                    }
                    config
                        .plugins
                        .get(&plugin.id)
                        .and_then(|settings| settings.command_action_words.get(&command.id))
                        .unwrap_or(&command.action_words)
                        .iter()
                        .any(|word| word.eq_ignore_ascii_case(candidate))
                })
            })
    };
    if words.iter().any(|word| collides(word)) {
        return Err(anyhow!("one or more trigger words are already in use"));
    }
    Ok(())
}

fn parse_bool(value: &str) -> Result<bool> {
    value
        .parse::<bool>()
        .with_context(|| format!("expected a boolean, got {value:?}"))
}

fn parse_i32(value: &str, id: &str) -> Result<i32> {
    value
        .parse::<i32>()
        .with_context(|| format!("invalid integer for {id}: {value:?}"))
}

fn parse_u32(value: &str, id: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .with_context(|| format!("invalid unsigned integer for {id}: {value:?}"))
}

fn plugin_category(id: &str) -> String {
    format!("plugin/{id}")
}

fn translate(language: &str, text: &'static str) -> String {
    i18n::tr_owned_in_context(language, "Settings", text)
}
