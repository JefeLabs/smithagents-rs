//! The create screen: a text box, the repository the intent goes in, and one
//! row per kind ([`KIND_CHOICES`]). Choosing a row, or pressing its ⌥ key
//! while the focus is on the screen, creates an intent of that kind from the
//! text as typed; the daemon then polishes it.
//!
//! The rows are disabled while the text is blank, while there is no
//! repository and while a command is in flight; the ⌥ keys then do nothing.
//! A key binding takes its key before the text box can type the key's
//! character (⌥B types "∫" on macOS), so the ⌥ keys never type into the box.
//!
//! The repository is the one picked in the menu (two or more repositories),
//! else the last one used, else the oldest ([`default_repo`]). A created
//! intent remembers its repository as the last used, clears the box and
//! opens its detail. A refused one keeps the text and shows the error line
//! under the rows. Close and Escape go back to the list; the text waits for
//! the next visit.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Escape, Textarea, TextareaState};
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Disableable as _, Icon, Sizable as _};
use gpui_kit::{
    div, rems, AnyElement, App, AppContext as _, Context, ElementId, Entity, FontWeight,
    InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, Role,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, WeakEntity, Window,
};
use smithagents_core::ids::RepoId;
use smithagents_core::intent::{CreateIntentRequest, Intent, IntentKind, SeedInput};

use super::{Busy, IntentsPane, Paint, Screen};
use crate::intent_vm::{default_repo, KindChoice, KIND_CHOICES};
use crate::view_models::{
    repo_choices, RepoChoiceVm, SettingsSection, NEED_REPO_TEXT, NOT_CONNECTED_TEXT,
};

gpui_kit::actions!(
    smithagents_intents,
    [
        /// On the create screen: create the intent as a new feature.
        ChooseNewFeature,
        /// On the create screen: create the intent as an enhancement.
        ChooseEnhancement,
        /// On the create screen: create the intent as a bug.
        ChooseBug,
        /// On the create screen: create the intent as a refactor.
        ChooseRefactor,
        /// On the create screen: create the intent as maintenance.
        ChooseMaintenance,
    ]
);

/// Key context of the create screen, for its ⌥ keys and Escape.
pub const CREATE_KEY_CONTEXT: &str = "IntentCreate";
/// Element id of the create screen.
pub const CREATE_SCREEN_ID: &str = "intent-create";
/// Element id of the create screen's heading.
pub const CREATE_TITLE_ID: &str = "intent-create-title";
/// Element id of the frame around the text box (the box itself is named
/// "Describe what you want").
pub const CREATE_INPUT_ID: &str = "intent-create-text";
/// Element id of the Close button.
pub const CREATE_CLOSE_ID: &str = "intent-create-close";
/// Element id of the repository line: a label, the repository menu's button
/// (two or more repositories), or the way to Settings (none).
pub const CREATE_REPO_ID: &str = "intent-create-repo";
/// Element ids of a kind row's parts, inside the row ([`kind_row_id`]).
pub const KIND_TITLE_ID: &str = "title";
pub const KIND_DESCRIPTION_ID: &str = "description";
pub const KIND_SHORTCUT_ID: &str = "shortcut";

const TITLE: &str = "New intent";
const INPUT_LABEL: &str = "Describe what you want";
const INPUT_PLACEHOLDER: &str = "Describe what you want…";
/// The text box grows from this many lines to this many, then scrolls.
const INPUT_MIN_ROWS: usize = 3;
const INPUT_MAX_ROWS: usize = 12;
const CLOSE_LABEL: &str = "Close";
/// The repository menu's label.
const REPO_MENU_LABEL: &str = "New intent in";
/// The repository menu's widest, in rems.
const REPO_MENU_MAX_WIDTH: f32 = 26.;

/// Element id of `kind`'s row.
pub fn kind_row_id(kind: IntentKind) -> ElementId {
    ElementId::Name(format!("intent-kind-{}", kind.wire_name()).into())
}

/// Element id of `repo`'s entry in the repository menu.
pub fn create_repo_item_id(repo: RepoId) -> ElementId {
    ElementId::Name(format!("intent-create-repo-{repo}").into())
}

/// The create screen's keys, in [`CREATE_KEY_CONTEXT`] only: each kind's ⌥
/// key ([`KindChoice::key`]) and Escape. The text box handles Escape itself
/// and passes it on; on any other control of the screen Escape comes from
/// here.
pub(super) fn key_bindings() -> Vec<KeyBinding> {
    let context = Some(CREATE_KEY_CONTEXT);
    KIND_CHOICES
        .iter()
        .filter_map(|choice| {
            let key = choice.key;
            Some(match choice.kind {
                IntentKind::NewFeature => KeyBinding::new(key, ChooseNewFeature, context),
                IntentKind::Enhancement => KeyBinding::new(key, ChooseEnhancement, context),
                IntentKind::Bug => KeyBinding::new(key, ChooseBug, context),
                IntentKind::Refactor => KeyBinding::new(key, ChooseRefactor, context),
                IntentKind::Maintenance => KeyBinding::new(key, ChooseMaintenance, context),
                // Not offered.
                IntentKind::Task => return None,
            })
        })
        .chain([KeyBinding::new("escape", Escape, context)])
        .collect()
}

/// The text box; Enter adds a line.
pub(super) fn new_text_box(window: &mut Window, cx: &mut App) -> Entity<TextareaState> {
    cx.new(|cx| {
        TextareaState::new(window, cx)
            .auto_grow(INPUT_MIN_ROWS, INPUT_MAX_ROWS)
            .placeholder(INPUT_PLACEHOLDER)
    })
}

/// What the repository line shows.
enum RepoLine {
    /// No repository yet: the way to Settings.
    Missing,
    /// One: its name.
    One(String),
    /// Several: the chosen one's name, and a menu of all.
    Several {
        chosen: String,
        choices: Vec<RepoChoiceVm>,
    },
}

impl IntentsPane {
    /// The repository a create goes in: the one picked in the menu while it
    /// exists, else the last used while it exists, else the oldest.
    fn create_repo(&self, cx: &App) -> Option<RepoId> {
        let state = self.state.read(cx);
        let repos = state.model().repos();
        self.picked_repo
            .filter(|id| repos.iter().any(|repo| repo.id == *id))
            .or_else(|| default_repo(repos, state.last_repo_id()))
    }

    fn repo_line(&self, cx: &App) -> RepoLine {
        let Some(chosen) = self.create_repo(cx) else {
            return RepoLine::Missing;
        };
        let repos = self.state.read(cx).model().repos();
        let order: Vec<RepoId> = repos.iter().map(|repo| repo.id).collect();
        let choices = repo_choices(repos, &order, chosen);
        let title = choices
            .iter()
            .find(|choice| choice.preselected)
            .map(|choice| choice.title.clone())
            .unwrap_or_default();
        if choices.len() > 1 {
            RepoLine::Several {
                chosen: title,
                choices,
            }
        } else {
            RepoLine::One(title)
        }
    }

    fn pick_repo(&mut self, repo: RepoId, cx: &mut Context<Self>) {
        tracing::debug!(%repo, "new intent in");
        self.picked_repo = Some(repo);
        cx.notify();
    }

    /// Creates an intent of `kind` from the text as typed, in the chosen
    /// repository. Does nothing while the text is blank, without a
    /// repository, or while a command is in flight: the rows are disabled
    /// then, and the ⌥ keys come here too.
    fn choose_kind(&mut self, kind: IntentKind, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text_box.read(cx).value().to_string();
        let repo = self.create_repo(cx);
        let Some(repo) = repo.filter(|_| !text.trim().is_empty() && self.busy.is_none()) else {
            tracing::debug!(?kind, "no intent to create yet");
            return;
        };
        let request = CreateIntentRequest {
            repo_id: Some(repo),
            kind: Some(kind),
            seed: SeedInput::Text(text.clone()),
        };
        let Some(command) = self
            .state
            .read(cx)
            .intent_commands()
            .map(|commands| commands.create_intent(request))
        else {
            self.error = Some(NOT_CONNECTED_TEXT.to_owned());
            cx.notify();
            return;
        };
        tracing::debug!(?kind, %repo, "creating an intent");
        self.creating_kind = Some(kind);
        self.run(
            Busy::Create,
            command,
            move |this, intent, window, cx| this.created(repo, &text, intent, window, cx),
            window,
            cx,
        );
    }

    /// The create of `sent` in `repo` succeeded. The repository becomes the
    /// last used. The box (and the repository picked) is cleared only while
    /// it still holds the text sent: text typed since then is kept for the
    /// next intent. The detail opens only while the create screen is on
    /// screen: a user who left it is not pulled away.
    fn created(
        &mut self,
        repo: RepoId,
        sent: &str,
        intent: Intent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state
            .update(cx, |state, _| state.set_last_repo_id(repo));
        if self.text_box.read(cx).value() == sent {
            self.picked_repo = None;
            // Unlike `set_value`, `clean` leaves the focus where it is.
            self.text_box
                .update(cx, |text_box, cx| text_box.clean(window, cx));
        }
        if self.screen == Screen::Create {
            self.open_screen(Screen::Detail(intent.id), window, cx);
        } else {
            tracing::debug!(
                intent = %intent.id,
                screen = ?self.screen,
                "created after the create screen was left"
            );
            cx.notify();
        }
    }

    pub(super) fn create_screen(&self, paint: &Paint, cx: &Context<Self>) -> impl IntoElement {
        let blank = self.text_box.read(cx).value().trim().is_empty();
        let repo_line = self.repo_line(cx);
        let enabled = !blank && !matches!(repo_line, RepoLine::Missing) && self.busy.is_none();
        let creating = self
            .creating_kind
            .filter(|_| self.busy == Some(Busy::Create));
        v_flex()
            .id(CREATE_SCREEN_ID)
            .test_support()
            .key_context(CREATE_KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &ChooseNewFeature, window, cx| {
                this.choose_kind(IntentKind::NewFeature, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ChooseEnhancement, window, cx| {
                this.choose_kind(IntentKind::Enhancement, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ChooseBug, window, cx| {
                this.choose_kind(IntentKind::Bug, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ChooseRefactor, window, cx| {
                this.choose_kind(IntentKind::Refactor, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ChooseMaintenance, window, cx| {
                this.choose_kind(IntentKind::Maintenance, window, cx)
            }))
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                this.open_screen(Screen::List, window, cx)
            }))
            .size_full()
            .child(self.create_header(paint, cx))
            .child(
                v_flex()
                    .id("intent-create-body")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .w_full()
                            .px_3()
                            .pb_2()
                            .gap_2()
                            .child(
                                div().id(CREATE_INPUT_ID).test_support().w_full().child(
                                    Textarea::new(&self.text_box)
                                        .aria_label(INPUT_LABEL)
                                        .small(),
                                ),
                            )
                            // A row, so a button keeps its own width.
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .child(self.repo_line_element(repo_line, paint, cx)),
                            )
                            .child(
                                v_flex()
                                    .w_full()
                                    .gap_1p5()
                                    .children(KIND_CHOICES.iter().map(|choice| {
                                        let loading = creating == Some(choice.kind);
                                        self.kind_row(*choice, enabled, loading, paint, cx)
                                    })),
                            ),
                    )
                    .children(self.error_line(paint)),
            )
    }

    fn create_header(&self, paint: &Paint, cx: &Context<Self>) -> impl IntoElement {
        h_flex()
            .flex_none()
            .w_full()
            .items_center()
            .justify_between()
            .gap_2()
            .pl_3()
            .pr_1()
            .py_2()
            .child(
                div()
                    .id(CREATE_TITLE_ID)
                    // A node needs an id and a role to reach assistive
                    // technology.
                    .role(Role::Heading)
                    .test_support()
                    .aria_label(TITLE)
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(paint.foreground)
                    .child(TITLE),
            )
            .child(
                Button::new(CREATE_CLOSE_ID)
                    .icon(Icon::new(IconName::X).text_color(paint.foreground))
                    .ghost()
                    .small()
                    .tooltip(CLOSE_LABEL)
                    .accessibility_label(CLOSE_LABEL)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_screen(Screen::List, window, cx)
                    })),
            )
    }

    /// "In <repository>": a muted label, or with two or more repositories
    /// a small button opening the repository menu; without a repository, a
    /// button to Settings → Source repositories.
    fn repo_line_element(&self, line: RepoLine, paint: &Paint, cx: &Context<Self>) -> AnyElement {
        let muted = paint.muted_foreground;
        match line {
            RepoLine::Missing => Button::new(CREATE_REPO_ID)
                .label(NEED_REPO_TEXT)
                .ghost()
                .xsmall()
                .text_color(muted)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.update(cx, |state, cx| {
                        state.request_settings_focus(SettingsSection::Repositories, cx)
                    })
                }))
                .into_any_element(),
            RepoLine::One(title) => Label::new(format!("In {title}"))
                .id(CREATE_REPO_ID)
                .min_w_0()
                .text_xs()
                .text_color(muted)
                .truncate()
                .into_any_element(),
            RepoLine::Several { chosen, choices } => {
                let pane = cx.entity().downgrade();
                Button::new(CREATE_REPO_ID)
                    .label(format!("In {chosen}"))
                    .ghost()
                    .xsmall()
                    .dropdown_caret(true)
                    .text_color(muted)
                    .dropdown_menu(move |menu, window, cx| {
                        repo_menu(menu, &choices, &pane, window, cx)
                    })
                    .into_any_element()
            }
        }
    }

    /// One kind: its title over its description, and its shortcut. A button
    /// named "<title>, <description>".
    fn kind_row(
        &self,
        choice: KindChoice,
        enabled: bool,
        loading: bool,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let kind = choice.kind;
        let muted = paint.muted_foreground;
        Button::new(kind_row_id(kind))
            .outline()
            .w_full()
            .h_auto()
            .py_1p5()
            .accessibility_label(format!("{}, {}", choice.title, choice.description))
            .disabled(!enabled)
            .loading(loading)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                Label::new(choice.title)
                                    .id(KIND_TITLE_ID)
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(if enabled { paint.foreground } else { muted })
                                    .truncate(),
                            )
                            .child(
                                Label::new(choice.description)
                                    .id(KIND_DESCRIPTION_ID)
                                    .text_xs()
                                    .text_color(muted)
                                    .truncate(),
                            ),
                    )
                    .child(
                        Label::new(choice.shortcut)
                            .id(KIND_SHORTCUT_ID)
                            .flex_none()
                            .text_xs()
                            .text_color(muted),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.choose_kind(kind, window, cx)))
    }
}

/// The repository menu: one entry per repository, its folder's name over
/// its full path, the chosen one checked.
fn repo_menu(
    mut menu: PopupMenu,
    choices: &[RepoChoiceVm],
    pane: &WeakEntity<IntentsPane>,
    window: &Window,
    cx: &App,
) -> PopupMenu {
    let muted = cx.theme().muted_foreground;
    // The menu takes its width in pixels; it follows the text size.
    let max_width = rems(REPO_MENU_MAX_WIDTH).to_pixels(window.rem_size());
    menu = menu.label(REPO_MENU_LABEL).max_w(max_width);
    for choice in choices {
        let (title, path, repo) = (choice.title.clone(), choice.path.clone(), choice.id);
        let pane = pane.clone();
        menu = menu.item(
            // gpui-component gives an element item's menu-item node no name
            // (`PopupMenuItem::a11y_label` is `None` for it), so the entry
            // carries its own named node; its two lines are plain text, so a
            // screen reader does not read them a second time.
            PopupMenuItem::element(move |_, _| {
                v_flex()
                    .id(create_repo_item_id(repo))
                    .role(Role::Group)
                    .test_support()
                    .aria_label(format!("{title}, {path}"))
                    .min_w_0()
                    .py_0p5()
                    .child(div().text_sm().child(title.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .truncate()
                            .child(path.clone()),
                    )
            })
            .checked(choice.preselected)
            .on_click(move |_, _, cx| {
                let _ = pane.update(cx, |pane, cx| pane.pick_repo(repo, cx));
            }),
        );
    }
    menu
}
