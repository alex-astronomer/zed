use anyhow::Result;
use editor::{scroll::Autoscroll, Editor};
use gpui::{
    div, list, prelude::*, uniform_list, AnyElement, AppContext, ClickEvent, DismissEvent,
    EventEmitter, FocusHandle, FocusableView, Length, ListState, Render, Task,
    UniformListScrollHandle, View, ViewContext, WindowContext,
};
use head::Head;
use search::SearchOptions;
use std::{sync::Arc, time::Duration};
use ui::{prelude::*, v_flex, Color, Divider, Label, ListItem, ListItemSpacing};
use workspace::ModalView;

mod head;
pub mod highlighted_match_with_paths;

enum ElementContainer {
    List(ListState),
    UniformList(UniformListScrollHandle),
}

struct PendingUpdateMatches {
    delegate_update_matches: Option<Task<()>>,
    _task: Task<Result<()>>,
}

pub struct Picker<D: PickerDelegate> {
    pub delegate: D,
    element_container: ElementContainer,
    head: Head,
    pending_update_matches: Option<PendingUpdateMatches>,
    confirm_on_update: Option<bool>,
    width: Option<Length>,
    max_height: Option<Length>,

    /// Whether the `Picker` is rendered as a self-contained modal.
    ///
    /// Set this to `false` when rendering the `Picker` as part of a larger modal.
    is_modal: bool,
}

#[derive(Copy, Clone)]
pub struct SupportedSearchOptions {
    include_ignored: bool,
}

impl SupportedSearchOptions {
    pub fn new(include_ignored: bool) -> Self {
        Self { include_ignored }
    }

    pub fn default() -> Self {
        Self::new(false)
    }
}

pub trait PickerDelegate: Sized + 'static {
    type ListItem: IntoElement;

    fn search_options(&self) -> SearchOptions;
    fn supported_search_options(&self) -> SupportedSearchOptions;

    fn toggle_include_ignored(&mut self) {}

    fn match_count(&self) -> usize;
    fn selected_index(&self) -> usize;
    fn separators_after_indices(&self) -> Vec<usize> {
        Vec::new()
    }
    fn set_selected_index(&mut self, ix: usize, cx: &mut ViewContext<Picker<Self>>);

    fn placeholder_text(&self, _cx: &mut WindowContext) -> Arc<str>;
    fn update_matches(&mut self, query: String, cx: &mut ViewContext<Picker<Self>>) -> Task<()>;

    // Delegates that support this method (e.g. the CommandPalette) can chose to block on any background
    // work for up to `duration` to try and get a result synchronously.
    // This avoids a flash of an empty command-palette on cmd-shift-p, and lets workspace::SendKeystrokes
    // mostly work when dismissing a palette.
    fn finalize_update_matches(
        &mut self,
        _query: String,
        _duration: Duration,
        _cx: &mut ViewContext<Picker<Self>>,
    ) -> bool {
        false
    }

    fn confirm(&mut self, secondary: bool, cx: &mut ViewContext<Picker<Self>>);
    fn dismissed(&mut self, cx: &mut ViewContext<Picker<Self>>);
    fn selected_as_query(&self) -> Option<String> {
        None
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        cx: &mut ViewContext<Picker<Self>>,
    ) -> Option<Self::ListItem>;
    fn render_header(&self, _: &mut ViewContext<Picker<Self>>) -> Option<AnyElement> {
        None
    }
    fn render_footer(&self, _: &mut ViewContext<Picker<Self>>) -> Option<AnyElement> {
        None
    }
}

impl<D: PickerDelegate> FocusableView for Picker<D> {
    fn focus_handle(&self, cx: &AppContext) -> FocusHandle {
        match &self.head {
            Head::Editor(editor) => editor.focus_handle(cx),
            Head::Empty(head) => head.focus_handle(cx),
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
enum ContainerKind {
    List,
    UniformList,
}

impl<D: PickerDelegate> Picker<D> {
    /// A picker, which displays its matches using `gpui::uniform_list`, all matches should have the same height.
    /// The picker allows the user to perform search items by text.
    /// If `PickerDelegate::render_match` can return items with different heights, use `Picker::list`.
    pub fn uniform_list(delegate: D, cx: &mut ViewContext<Self>) -> Self {
        let head = Head::editor(
            delegate.placeholder_text(cx),
            Self::on_input_editor_event,
            cx,
        );

        Self::new(delegate, ContainerKind::UniformList, head, cx)
    }

    /// A picker, which displays its matches using `gpui::uniform_list`, all matches should have the same height.
    /// If `PickerDelegate::render_match` can return items with different heights, use `Picker::list`.
    pub fn nonsearchable_uniform_list(delegate: D, cx: &mut ViewContext<Self>) -> Self {
        let head = Head::empty(cx);

        Self::new(delegate, ContainerKind::UniformList, head, cx)
    }

    /// A picker, which displays its matches using `gpui::list`, matches can have different heights.
    /// The picker allows the user to perform search items by text.
    /// If `PickerDelegate::render_match` only returns items with the same height, use `Picker::uniform_list` as its implementation is optimized for that.
    pub fn list(delegate: D, cx: &mut ViewContext<Self>) -> Self {
        let head = Head::editor(
            delegate.placeholder_text(cx),
            Self::on_input_editor_event,
            cx,
        );

        Self::new(delegate, ContainerKind::List, head, cx)
    }

    fn new(delegate: D, container: ContainerKind, head: Head, cx: &mut ViewContext<Self>) -> Self {
        let mut this = Self {
            delegate,
            head,
            element_container: Self::create_element_container(container, cx),
            pending_update_matches: None,
            confirm_on_update: None,
            width: None,
            max_height: None,
            is_modal: true,
        };
        this.update_matches("".to_string(), cx);
        // give the delegate 4ms to render the first set of suggestions.
        this.delegate
            .finalize_update_matches("".to_string(), Duration::from_millis(4), cx);
        this
    }

    fn create_element_container(
        container: ContainerKind,
        cx: &mut ViewContext<Self>,
    ) -> ElementContainer {
        match container {
            ContainerKind::UniformList => {
                ElementContainer::UniformList(UniformListScrollHandle::new())
            }
            ContainerKind::List => {
                let view = cx.view().downgrade();
                ElementContainer::List(ListState::new(
                    0,
                    gpui::ListAlignment::Top,
                    px(1000.),
                    move |ix, cx| {
                        view.upgrade()
                            .map(|view| {
                                view.update(cx, |this, cx| {
                                    this.render_element(cx, ix).into_any_element()
                                })
                            })
                            .unwrap_or_else(|| div().into_any_element())
                    },
                ))
            }
        }
    }

    pub fn width(mut self, width: impl Into<gpui::Length>) -> Self {
        self.width = Some(width.into());
        self
    }

    pub fn max_height(mut self, max_height: impl Into<gpui::Length>) -> Self {
        self.max_height = Some(max_height.into());
        self
    }

    pub fn modal(mut self, modal: bool) -> Self {
        self.is_modal = modal;
        self
    }

    pub fn focus(&self, cx: &mut WindowContext) {
        self.focus_handle(cx).focus(cx);
    }

    pub fn select_next(&mut self, _: &menu::SelectNext, cx: &mut ViewContext<Self>) {
        let count = self.delegate.match_count();
        if count > 0 {
            let index = self.delegate.selected_index();
            let ix = if index == count - 1 { 0 } else { index + 1 };
            self.delegate.set_selected_index(ix, cx);
            self.scroll_to_item_index(ix);
            cx.notify();
        }
    }

    fn select_prev(&mut self, _: &menu::SelectPrev, cx: &mut ViewContext<Self>) {
        let count = self.delegate.match_count();
        if count > 0 {
            let index = self.delegate.selected_index();
            let ix = if index == 0 { count - 1 } else { index - 1 };
            self.delegate.set_selected_index(ix, cx);
            self.scroll_to_item_index(ix);
            cx.notify();
        }
    }

    fn select_first(&mut self, _: &menu::SelectFirst, cx: &mut ViewContext<Self>) {
        let count = self.delegate.match_count();
        if count > 0 {
            self.delegate.set_selected_index(0, cx);
            self.scroll_to_item_index(0);
            cx.notify();
        }
    }

    fn select_last(&mut self, _: &menu::SelectLast, cx: &mut ViewContext<Self>) {
        let count = self.delegate.match_count();
        if count > 0 {
            self.delegate.set_selected_index(count - 1, cx);
            self.scroll_to_item_index(count - 1);
            cx.notify();
        }
    }

    pub fn cycle_selection(&mut self, cx: &mut ViewContext<Self>) {
        let count = self.delegate.match_count();
        let index = self.delegate.selected_index();
        let new_index = if index + 1 == count { 0 } else { index + 1 };
        self.delegate.set_selected_index(new_index, cx);
        self.scroll_to_item_index(new_index);
        cx.notify();
    }

    pub fn cancel(&mut self, _: &menu::Cancel, cx: &mut ViewContext<Self>) {
        self.delegate.dismissed(cx);
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &menu::Confirm, cx: &mut ViewContext<Self>) {
        if self.pending_update_matches.is_some()
            && !self
                .delegate
                .finalize_update_matches(self.query(cx), Duration::from_millis(16), cx)
        {
            self.confirm_on_update = Some(false)
        } else {
            self.pending_update_matches.take();
            self.delegate.confirm(false, cx);
        }
    }

    fn secondary_confirm(&mut self, _: &menu::SecondaryConfirm, cx: &mut ViewContext<Self>) {
        if self.pending_update_matches.is_some()
            && !self
                .delegate
                .finalize_update_matches(self.query(cx), Duration::from_millis(16), cx)
        {
            self.confirm_on_update = Some(true)
        } else {
            self.delegate.confirm(true, cx);
        }
    }

    fn use_selected_query(&mut self, _: &menu::UseSelectedQuery, cx: &mut ViewContext<Self>) {
        if let Some(new_query) = self.delegate.selected_as_query() {
            self.set_query(new_query, cx);
            cx.stop_propagation();
        }
    }

    fn handle_click(&mut self, ix: usize, secondary: bool, cx: &mut ViewContext<Self>) {
        cx.stop_propagation();
        cx.prevent_default();
        self.delegate.set_selected_index(ix, cx);
        self.delegate.confirm(secondary, cx);
    }

    fn on_input_editor_event(
        &mut self,
        _: View<Editor>,
        event: &editor::EditorEvent,
        cx: &mut ViewContext<Self>,
    ) {
        let Head::Editor(ref editor) = &self.head else {
            panic!("unexpected call");
        };
        match event {
            editor::EditorEvent::BufferEdited => {
                let query = editor.read(cx).text(cx);
                self.update_matches(query, cx);
            }
            editor::EditorEvent::Blurred => {
                self.cancel(&menu::Cancel, cx);
            }
            _ => {}
        }
    }

    pub fn refresh(&mut self, cx: &mut ViewContext<Self>) {
        let query = self.query(cx);
        self.update_matches(query, cx);
    }

    pub fn update_matches(&mut self, query: String, cx: &mut ViewContext<Self>) {
        let delegate_pending_update_matches = self.delegate.update_matches(query, cx);

        self.matches_updated(cx);
        // This struct ensures that we can synchronously drop the task returned by the
        // delegate's `update_matches` method and the task that the picker is spawning.
        // If we simply capture the delegate's task into the picker's task, when the picker's
        // task gets synchronously dropped, the delegate's task would keep running until
        // the picker's task has a chance of being scheduled, because dropping a task happens
        // asynchronously.
        self.pending_update_matches = Some(PendingUpdateMatches {
            delegate_update_matches: Some(delegate_pending_update_matches),
            _task: cx.spawn(|this, mut cx| async move {
                let delegate_pending_update_matches = this.update(&mut cx, |this, _| {
                    this.pending_update_matches
                        .as_mut()
                        .unwrap()
                        .delegate_update_matches
                        .take()
                        .unwrap()
                })?;
                delegate_pending_update_matches.await;
                this.update(&mut cx, |this, cx| {
                    this.matches_updated(cx);
                })
            }),
        });
    }

    fn matches_updated(&mut self, cx: &mut ViewContext<Self>) {
        if let ElementContainer::List(state) = &mut self.element_container {
            state.reset(self.delegate.match_count());
        }

        let index = self.delegate.selected_index();
        self.scroll_to_item_index(index);
        self.pending_update_matches = None;
        if let Some(secondary) = self.confirm_on_update.take() {
            self.delegate.confirm(secondary, cx);
        }
        cx.notify();
    }

    pub fn query(&self, cx: &AppContext) -> String {
        match &self.head {
            Head::Editor(editor) => editor.read(cx).text(cx),
            Head::Empty(_) => "".to_string(),
        }
    }

    pub fn set_query(&self, query: impl Into<Arc<str>>, cx: &mut ViewContext<Self>) {
        if let Head::Editor(ref editor) = &self.head {
            editor.update(cx, |editor, cx| {
                editor.set_text(query, cx);
                let editor_offset = editor.buffer().read(cx).len(cx);
                editor.change_selections(Some(Autoscroll::Next), cx, |s| {
                    s.select_ranges(Some(editor_offset..editor_offset))
                });
            });
        }
    }

    fn scroll_to_item_index(&mut self, ix: usize) {
        match &mut self.element_container {
            ElementContainer::List(state) => state.scroll_to_reveal_item(ix),
            ElementContainer::UniformList(scroll_handle) => scroll_handle.scroll_to_item(ix),
        }
    }

    fn render_element(&self, cx: &mut ViewContext<Self>, ix: usize) -> impl IntoElement {
        div()
            .id(("item", ix))
            .cursor_pointer()
            .on_click(cx.listener(move |this, event: &ClickEvent, cx| {
                this.handle_click(ix, event.down.modifiers.command, cx)
            }))
            .children(
                self.delegate
                    .render_match(ix, ix == self.delegate.selected_index(), cx),
            )
            .when(
                self.delegate.separators_after_indices().contains(&ix),
                |picker| {
                    picker
                        .border_color(cx.theme().colors().border_variant)
                        .border_b_1()
                        .pb(px(-1.0))
                },
            )
    }

    fn render_element_container(&self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        match &self.element_container {
            ElementContainer::UniformList(scroll_handle) => uniform_list(
                cx.view().clone(),
                "candidates",
                self.delegate.match_count(),
                move |picker, visible_range, cx| {
                    visible_range
                        .map(|ix| picker.render_element(cx, ix))
                        .collect()
                },
            )
            .py_2()
            .track_scroll(scroll_handle.clone())
            .into_any_element(),
            ElementContainer::List(state) => list(state.clone())
                .with_sizing_behavior(gpui::ListSizingBehavior::Infer)
                .py_2()
                .into_any_element(),
        }
    }

    fn render_search_buttons(&self, cx: &mut ViewContext<Self>) -> Vec<impl IntoElement> {
        let mut buttons = vec![];
        if self.delegate.supported_search_options().include_ignored {
            buttons.push(
                SearchOptions::INCLUDE_IGNORED.as_button(
                    self.delegate
                        .search_options()
                        .contains(SearchOptions::INCLUDE_IGNORED),
                    cx.listener(|this, _, cx| {
                        this.delegate.toggle_include_ignored();
                        cx.notify();
                        this.update_matches(this.query(cx).to_string(), cx);
                    }),
                ),
            );
        }
        buttons
    }
}

impl<D: PickerDelegate> EventEmitter<DismissEvent> for Picker<D> {}
impl<D: PickerDelegate> ModalView for Picker<D> {}

impl<D: PickerDelegate> Render for Picker<D> {
    fn render(&mut self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        div()
            .key_context("Picker")
            .size_full()
            .when_some(self.width, |el, width| el.w(width))
            .overflow_hidden()
            // This is a bit of a hack to remove the modal styling when we're rendering the `Picker`
            // as a part of a modal rather than the entire modal.
            //
            // We should revisit how the `Picker` is styled to make it more composable.
            .when(self.is_modal, |this| this.elevation_3(cx))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::secondary_confirm))
            .on_action(cx.listener(Self::use_selected_query))
            .child(match &self.head {
                Head::Editor(editor) => v_flex()
                    .child(
                        h_flex()
                            .overflow_hidden()
                            .flex_none()
                            .h_9()
                            .px_4()
                            .child(editor.clone())
                            .children(self.render_search_buttons(cx)),
                    )
                    .child(Divider::horizontal()),
                Head::Empty(empty_head) => div().child(empty_head.clone()),
            })
            .when(self.delegate.match_count() > 0, |el| {
                el.child(
                    v_flex()
                        .flex_grow()
                        .max_h(self.max_height.unwrap_or(rems(18.).into()))
                        .overflow_hidden()
                        .children(self.delegate.render_header(cx))
                        .child(self.render_element_container(cx)),
                )
            })
            .when(self.delegate.match_count() == 0, |el| {
                el.child(
                    v_flex().flex_grow().py_2().child(
                        ListItem::new("empty_state")
                            .inset(true)
                            .spacing(ListItemSpacing::Sparse)
                            .disabled(true)
                            .child(Label::new("No matches").color(Color::Muted)),
                    ),
                )
            })
            .children(self.delegate.render_footer(cx))
    }
}
