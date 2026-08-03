use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use ::gpui::prelude::FluentBuilder as _;
use ::gpui::{
    AnyElement, App, AppContext as _, AvailableSpace, Bounds, ClipboardItem, Context, Element,
    ElementId, Entity, FocusHandle, FontStyle, FontWeight, GlobalElementId, HighlightStyle, Hitbox,
    HitboxBehavior, Image, ImageFormat, ImageSource, InspectorElementId, InteractiveElement as _,
    InteractiveText, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ObjectFit, ParentElement as _, Pixels, Point, Render, RenderImage, ScrollAnchor,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, StrikethroughStyle,
    StyleRefinement, Styled as _, StyledImage as _, StyledText, Task, TextLayout, UnderlineStyle,
    WeakEntity, Window, combine_highlights, div, img, point, px, size,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Selectable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    highlighter::{HighlightTheme, SyntaxHighlighter},
    input::{Copy as CopyAction, SelectAll},
    link::Link,
    progress::Progress,
    scroll::ScrollableElement as _,
    spinner::Spinner,
    v_flex,
};
use ropey::Rope;

use crate::artifact::{
    ArtifactController, ArtifactError, ArtifactKey, ArtifactKind, ArtifactRequest,
    ArtifactSchedule, ArtifactTheme, render_local_artifact_with_timeout,
};
use crate::limits::{MarkdownLimits, bounded_text, utf8_prefix};
use crate::model::{
    Block, BlockNode, CalloutKind, DiagramKind, Inline, InlineImage, InlineNode, MarkdownDocument,
    MarkdownInput, NodeId, TableAlignment,
};
use crate::parser::parse_markdown;
use crate::resource::{ResolvedResource, ResourceKind};
use crate::svg::{SvgArtifact, SvgPolicy};

const SYNCHRONOUS_PARSE_BYTES: usize = 16 * 1024;
const AGENT_BLOCK_VIRTUALIZATION_MIN_BLOCKS: usize = 24;
const AGENT_BLOCK_VIRTUALIZATION_MIN_SOURCE_BYTES: usize = 16 * 1024;
const AGENT_BLOCK_VIRTUALIZATION_MIN_LARGE_BLOCKS: usize = 8;
const AGENT_BLOCK_VIRTUALIZATION_OVERSCAN_PX: f32 = 640.0;
const AGENT_BLOCK_GAP_PX: f32 = 12.0;
const AGENT_BLOCK_ESTIMATE_WIDTH_PX: f32 = 720.0;
const CODE_HIGHLIGHT_TIMEOUT: Duration = Duration::from_millis(20);
const DATA_IMAGE_CACHE_ENTRIES: usize = 16;
const DATA_IMAGE_CACHE_BYTES: usize = 16 * 1024 * 1024;
const DATA_IMAGE_DECODED_BYTES: usize = 8 * 1024 * 1024;
const DATA_IMAGE_MAX_DIMENSION: u32 = 16_384;
const DATA_IMAGE_MAX_RGBA_BYTES: u64 = 64 * 1024 * 1024;
const ARTIFACT_MAX_RASTER_PIXELS: f32 = 16.0 * 1024.0 * 1024.0;

pub type MarkdownResourceHandler = Arc<dyn Fn(ResolvedResource, &mut Window, &mut App) + 'static>;
type MarkdownInlineClickHandler = Rc<dyn Fn(&mut Window, &mut App) + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MarkdownPresentation {
    #[default]
    Document,
    Agent,
    Thought,
}

#[derive(Clone, Default)]
pub struct MarkdownViewOptions {
    pub presentation: MarkdownPresentation,
    pub search_query: Option<Arc<str>>,
    pub images: Arc<BTreeMap<String, Arc<Image>>>,
    pub allow_http_images: bool,
    pub streaming: bool,
    pub scroll_handle: Option<ScrollHandle>,
    pub on_open_resource: Option<MarkdownResourceHandler>,
}

/// Compare two shared texts, taking the pointer shortcut first.
///
/// Callers hand the same `Arc<str>` back on every frame when nothing changed;
/// without this the equality check degrades into a full memcmp of the whole
/// message body once per frame per visible row.
fn markdown_text_matches(previous: &Arc<str>, next: &Arc<str>) -> bool {
    Arc::ptr_eq(previous, next) || previous == next
}

fn markdown_render_options_changed(
    previous: &MarkdownViewOptions,
    next: &MarkdownViewOptions,
) -> bool {
    previous.presentation != next.presentation
        || previous.search_query != next.search_query
        || previous.allow_http_images != next.allow_http_images
        || previous.streaming != next.streaming
        || previous.scroll_handle.is_some() != next.scroll_handle.is_some()
        || (!(previous.images.is_empty() && next.images.is_empty())
            && !Arc::ptr_eq(&previous.images, &next.images))
}

#[derive(Clone)]
pub struct MarkdownView {
    id: ElementId,
    view_id: Arc<str>,
    input: MarkdownInput,
    document: Option<Arc<MarkdownDocument>>,
    options: MarkdownViewOptions,
    style: StyleRefinement,
    state: Option<Entity<MarkdownViewState>>,
}

impl MarkdownView {
    pub fn new(id: impl Into<ElementId>, input: MarkdownInput) -> Self {
        let id = id.into();
        Self {
            view_id: Arc::from(format!("{id}")),
            id,
            input,
            document: None,
            options: MarkdownViewOptions::default(),
            style: StyleRefinement::default(),
            state: None,
        }
    }

    pub fn from_document(id: impl Into<ElementId>, document: Arc<MarkdownDocument>) -> Self {
        let input = MarkdownInput::new(
            document.source.clone(),
            document.base_path.clone(),
            document.revision,
        );
        let mut view = Self::new(id, input);
        view.document = Some(document);
        view
    }

    /// Render user-authored plain text with native selection without interpreting Markdown.
    pub fn plain_text(id: impl Into<ElementId>, input: MarkdownInput) -> Self {
        let source = input.source.clone();
        let range = crate::model::SourceRange::new(0, source.len());
        let document = Arc::new(MarkdownDocument {
            source: source.clone(),
            base_path: input.base_path.clone(),
            revision: input.revision,
            blocks: (!source.is_empty())
                .then(|| BlockNode {
                    id: NodeId(0),
                    range,
                    kind: Block::Paragraph(vec![InlineNode {
                        id: NodeId(1),
                        range,
                        kind: Inline::Text(source.to_string()),
                    }]),
                })
                .into_iter()
                .collect::<Vec<_>>()
                .into(),
            outline: Arc::default(),
            footnotes: Default::default(),
            resources: Arc::default(),
            diagnostics: Arc::default(),
            truncated: false,
        });
        Self::from_document(id, document)
    }

    pub fn presentation(mut self, presentation: MarkdownPresentation) -> Self {
        self.options.presentation = presentation;
        self
    }

    pub fn search_query(mut self, query: Option<impl Into<Arc<str>>>) -> Self {
        self.options.search_query = query.map(Into::into);
        self
    }

    pub fn images(mut self, images: Arc<BTreeMap<String, Arc<Image>>>) -> Self {
        self.options.images = images;
        self
    }

    pub fn allow_http_images(mut self, allow: bool) -> Self {
        self.options.allow_http_images = allow;
        self
    }

    pub fn streaming(mut self, streaming: bool) -> Self {
        self.options.streaming = streaming;
        self
    }

    pub fn scroll_handle(mut self, scroll_handle: ScrollHandle) -> Self {
        self.options.scroll_handle = Some(scroll_handle);
        self
    }

    pub fn on_open_resource(
        mut self,
        handler: impl Fn(ResolvedResource, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.options.on_open_resource = Some(Arc::new(handler));
        self
    }
}

impl ::gpui::Styled for MarkdownView {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for MarkdownView {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

pub struct MarkdownViewLayoutState {
    state: Entity<MarkdownViewState>,
    element: AnyElement,
}

impl Element for MarkdownView {
    type RequestLayoutState = MarkdownViewLayoutState;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let state = if let Some(state) = self.state.clone() {
            state
        } else {
            let view_id = self.view_id.clone();
            let input = self.input.clone();
            let document = self.document.clone();
            let options = self.options.clone();
            let state = window.use_keyed_state(
                SharedString::from(format!("{}/markdown-state", self.view_id)),
                cx,
                move |_, cx| MarkdownViewState::new(view_id, input, document, options, cx),
            );
            self.state = Some(state.clone());
            state
        };
        let input = self.input.clone();
        let document = self.document.clone();
        let options = self.options.clone();
        state.update(cx, |state, cx| state.update(input, document, options, cx));
        let mut element = div()
            .key_context("MarkdownView")
            .w_full()
            .min_w_0()
            .child(state.clone())
            .refine_style(&self.style)
            .into_any_element();
        let layout_id = element.request_layout(window, cx);
        (layout_id, MarkdownViewLayoutState { state, element })
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        request_layout.element.prepaint(window, cx);
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        request_layout.element.paint(window, cx);

        let state = request_layout.state.downgrade();
        let selection_hitbox = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if event.button != MouseButton::Left {
                return;
            }
            if phase.capture() && !selection_hitbox.is_hovered(window) {
                let _ = state.update(cx, |state, cx| state.clear_text_selection(cx));
            } else if phase.bubble() && selection_hitbox.is_hovered(window) {
                let _ = state.update(cx, |state, cx| {
                    state.start_text_selection(event, window, cx)
                });
            }
        });

        let state = request_layout.state.downgrade();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase.capture() {
                let _ = state.update(cx, |state, cx| {
                    state.update_text_selection(event, window, cx)
                });
            }
        });

        let state = request_layout.state.downgrade();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase.capture() && event.button == MouseButton::Left {
                let _ = state.update(cx, |state, cx| state.finish_text_selection(window, cx));
            }
        });
    }
}

struct MarkdownVirtualFlow {
    state: Entity<MarkdownViewState>,
    total_height: Pixels,
}

impl MarkdownVirtualFlow {
    fn new(state: Entity<MarkdownViewState>, total_height: Pixels) -> Self {
        Self {
            state,
            total_height,
        }
    }
}

impl IntoElement for MarkdownVirtualFlow {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct MarkdownVirtualFlowLayoutState {
    spacer: AnyElement,
    blocks: Vec<AnyElement>,
}

impl Element for MarkdownVirtualFlow {
    type RequestLayoutState = MarkdownVirtualFlowLayoutState;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut spacer = div()
            .w_full()
            .min_w_0()
            .h(self.total_height)
            .flex_none()
            .into_any_element();
        let layout_id = spacer.request_layout(window, cx);
        (
            layout_id,
            MarkdownVirtualFlowLayoutState {
                spacer,
                blocks: Vec::new(),
            },
        )
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        layout.spacer.prepaint(window, cx);
        layout.blocks.clear();

        let viewport = window.content_mask().bounds;
        let width = bounds.size.width.max(px(1.0));
        let (visible_blocks, origins, blocks) = self.state.update(cx, |state, cx| {
            let layout_changed = state.prepare_virtual_layout(width);
            let visible_blocks = state.visible_virtual_blocks(bounds, viewport);
            state.prepare_virtual_selection(visible_blocks.clone());
            let origins = state.virtual_block_origins.clone();
            let blocks = visible_blocks
                .clone()
                .filter_map(|index| state.render_virtual_block(index, window, cx))
                .collect::<Vec<_>>();
            state.normalize_text_selection();
            if layout_changed {
                cx.notify();
            }
            (visible_blocks, origins, blocks)
        });

        let available_space = size(AvailableSpace::Definite(width), AvailableSpace::MinContent);
        let mut measurements = Vec::with_capacity(blocks.len());
        for (index, mut block) in visible_blocks.zip(blocks) {
            let measured = block.layout_as_root(available_space, window, cx);
            let origin = bounds.origin + point(px(0.0), origins[index]);
            block.prepaint_at(origin, window, cx);
            measurements.push((index, measured.height));
            layout.blocks.push(block);
        }
        drop(origins);

        if !measurements.is_empty() {
            self.state.update(cx, |state, cx| {
                if state.record_virtual_block_heights(width, &measurements) {
                    cx.notify();
                }
            });
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        layout.spacer.paint(window, cx);
        for block in &mut layout.blocks {
            block.paint(window, cx);
        }
    }
}

struct ArtifactSpec {
    node_id: NodeId,
    kind: ArtifactKind,
    source: Arc<str>,
}

enum ArtifactDisplayState {
    Loading {
        key: ArtifactKey,
    },
    Ready {
        key: ArtifactKey,
        image: Arc<RenderImage>,
        artifact: Arc<SvgArtifact>,
    },
    Failed {
        key: ArtifactKey,
        message: String,
    },
}

type HighlightCache = BTreeMap<(NodeId, bool), Arc<Vec<(Range<usize>, HighlightStyle)>>>;

struct CachedDataImage {
    image: Arc<Image>,
    bytes: usize,
    epoch: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct MarkdownTextSelection {
    anchor: usize,
    head: usize,
    pending: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct InlineSemanticStyle {
    strong: bool,
    emphasis: bool,
    deletion: bool,
    underline: bool,
}

#[derive(Default)]
struct InlineTextFlow {
    text: String,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    mono_ranges: Vec<Range<usize>>,
    actions: Vec<(Range<usize>, MarkdownInlineClickHandler)>,
}

struct MarkdownSelectableTextDecorations {
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    mono_ranges: Vec<Range<usize>>,
    actions: Vec<(Range<usize>, MarkdownInlineClickHandler)>,
    mono_font_family: SharedString,
}

impl InlineSemanticStyle {
    fn strong(mut self) -> Self {
        self.strong = true;
        self
    }

    fn emphasis(mut self) -> Self {
        self.emphasis = true;
        self
    }

    fn deletion(mut self) -> Self {
        self.deletion = true;
        self
    }

    fn underline(mut self) -> Self {
        self.underline = true;
        self
    }

    fn highlight(self) -> HighlightStyle {
        HighlightStyle {
            font_weight: self.strong.then_some(FontWeight::BOLD),
            font_style: self.emphasis.then_some(FontStyle::Italic),
            underline: self.underline.then_some(UnderlineStyle {
                thickness: px(1.0),
                ..Default::default()
            }),
            strikethrough: self.deletion.then_some(StrikethroughStyle {
                thickness: px(1.0),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

fn combine_inline_highlights(
    text_len: usize,
    semantic: InlineSemanticStyle,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let semantic = semantic.highlight();
    if text_len == 0 || semantic == HighlightStyle::default() {
        return highlights;
    }
    combine_highlights([(0..text_len, semantic)], highlights).collect()
}

fn inline_supports_text_flow(inline: &InlineNode) -> bool {
    match &inline.kind {
        Inline::Image(_)
        | Inline::Math(_)
        | Inline::Superscript(_)
        | Inline::Subscript(_)
        | Inline::FootnoteReference(_) => false,
        kind => kind
            .children()
            .is_none_or(|children| children.iter().all(inline_supports_text_flow)),
    }
}

fn merge_inline_ranges(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|range| (range.start, range.end));
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges.into_iter().filter(|range| !range.is_empty()) {
        if let Some(previous) = merged.last_mut()
            && range.start <= previous.end
        {
            previous.end = previous.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    merged
}

impl MarkdownTextSelection {
    fn range(self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
}

#[derive(Clone)]
struct SelectionSegment {
    frame: u64,
    text_range: Range<usize>,
    bounds: Bounds<Pixels>,
    layout: TextLayout,
}

pub struct MarkdownViewState {
    view_id: Arc<str>,
    focus_handle: FocusHandle,
    input: MarkdownInput,
    options: MarkdownViewOptions,
    document: Arc<MarkdownDocument>,
    parse_generation: u64,
    parse_task: Option<Task<()>>,
    live_nodes: BTreeSet<NodeId>,
    details_open: BTreeMap<NodeId, bool>,
    diagram_source: BTreeSet<NodeId>,
    outline_open: bool,
    anchors: BTreeMap<NodeId, ScrollAnchor>,
    artifact_specs: Vec<ArtifactSpec>,
    artifact_controller: ArtifactController,
    artifact_states: BTreeMap<NodeId, ArtifactDisplayState>,
    artifact_tasks: BTreeMap<ArtifactKey, Task<()>>,
    image_cache: BTreeMap<String, CachedDataImage>,
    image_cache_bytes: usize,
    image_cache_epoch: u64,
    highlight_cache: HighlightCache,
    selection_frame: u64,
    selection_next_segment: usize,
    selection_text: String,
    selection_segments: BTreeMap<usize, SelectionSegment>,
    text_selection: MarkdownTextSelection,
    virtual_block_sizes: Arc<Vec<Pixels>>,
    virtual_block_origins: Arc<Vec<Pixels>>,
    virtual_layout_width: Option<Pixels>,
    virtual_visible_blocks: Option<Range<usize>>,
    virtualized_selection: bool,
    select_all_document: bool,
}

impl MarkdownViewState {
    fn new(
        view_id: Arc<str>,
        input: MarkdownInput,
        document: Option<Arc<MarkdownDocument>>,
        options: MarkdownViewOptions,
        cx: &mut Context<Self>,
    ) -> Self {
        let parse_in_background = document.is_none()
            && (options.streaming || input.source.len() > SYNCHRONOUS_PARSE_BYTES);
        let background_input = parse_in_background.then(|| input.clone());
        let document = document.unwrap_or_else(|| {
            if parse_in_background {
                let mut fallback = input.clone();
                fallback.source = Arc::from(utf8_prefix(&input.source, SYNCHRONOUS_PARSE_BYTES));
                Arc::new(MarkdownDocument::literal(
                    &fallback,
                    "markdown_parse_pending",
                    "Markdown parsing is running in the background",
                ))
            } else {
                Arc::new(parse_markdown(input.clone()))
            }
        });
        let mut this = Self {
            view_id,
            focus_handle: cx.focus_handle(),
            input,
            options,
            document,
            parse_generation: 1,
            parse_task: None,
            live_nodes: BTreeSet::new(),
            details_open: BTreeMap::new(),
            diagram_source: BTreeSet::new(),
            outline_open: false,
            anchors: BTreeMap::new(),
            artifact_specs: Vec::new(),
            artifact_controller: ArtifactController::default(),
            artifact_states: BTreeMap::new(),
            artifact_tasks: BTreeMap::new(),
            image_cache: BTreeMap::new(),
            image_cache_bytes: 0,
            image_cache_epoch: 0,
            highlight_cache: BTreeMap::new(),
            selection_frame: 0,
            selection_next_segment: 0,
            selection_text: String::new(),
            selection_segments: BTreeMap::new(),
            text_selection: MarkdownTextSelection::default(),
            virtual_block_sizes: Arc::new(Vec::new()),
            virtual_block_origins: Arc::new(Vec::new()),
            virtual_layout_width: None,
            virtual_visible_blocks: None,
            virtualized_selection: false,
            select_all_document: false,
        };
        this.refresh_document_state();
        if let Some(input) = background_input {
            this.queue_background_parse(input, 1, cx);
        }
        cx.notify();
        this
    }

    fn update(
        &mut self,
        input: MarkdownInput,
        document: Option<Arc<MarkdownDocument>>,
        options: MarkdownViewOptions,
        cx: &mut Context<Self>,
    ) {
        let options_changed = markdown_render_options_changed(&self.options, &options);
        self.options = options;
        if let Some(document) = document {
            let changed = self.document.revision != document.revision
                || !markdown_text_matches(&self.document.source, &document.source)
                || !markdown_text_matches(&self.document.base_path, &document.base_path);
            self.input = input;
            if changed {
                self.parse_generation = self.parse_generation.saturating_add(1).max(1);
                self.parse_task = None;
                self.apply_document(document);
                cx.notify();
            } else if options_changed {
                self.rebuild_anchors();
                cx.notify();
            }
            return;
        }
        if self.input.revision == input.revision
            && markdown_text_matches(&self.input.source, &input.source)
            && markdown_text_matches(&self.input.base_path, &input.base_path)
        {
            if options_changed {
                self.rebuild_anchors();
                cx.notify();
            }
            return;
        }
        self.input = input.clone();
        self.parse_generation = self.parse_generation.saturating_add(1).max(1);
        let generation = self.parse_generation;
        if !self.options.streaming && input.source.len() <= SYNCHRONOUS_PARSE_BYTES {
            self.parse_task = None;
            self.apply_document(Arc::new(parse_markdown(input)));
            cx.notify();
            return;
        }
        self.queue_background_parse(input, generation, cx);
    }

    fn queue_background_parse(
        &mut self,
        input: MarkdownInput,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        if self.parse_task.is_some() {
            return;
        }
        let parse = cx.background_spawn(async move { Arc::new(parse_markdown(input)) });
        self.parse_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let document = parse.await;
            let _ = entity.update(cx, |this, cx| {
                this.parse_task = None;
                if this.parse_generation == generation
                    && this.input.revision == document.revision
                    && this.input.source == document.source
                    && this.input.base_path == document.base_path
                {
                    this.apply_document(document);
                    cx.notify();
                } else {
                    let latest = this.input.clone();
                    let latest_generation = this.parse_generation;
                    this.queue_background_parse(latest, latest_generation, cx);
                }
            });
        }));
    }

    fn apply_document(&mut self, document: Arc<MarkdownDocument>) {
        self.document = document;
        self.refresh_document_state();
    }

    fn refresh_document_state(&mut self) {
        self.text_selection = MarkdownTextSelection::default();
        self.select_all_document = false;
        self.selection_segments.clear();
        self.selection_text.clear();
        self.virtual_visible_blocks = None;
        self.virtual_layout_width = None;
        let mut live_nodes = BTreeSet::new();
        let mut artifact_specs = Vec::new();
        collect_document_state(
            &self.document.blocks,
            &mut live_nodes,
            &mut artifact_specs,
            MarkdownLimits::default().max_artifacts,
        );
        self.live_nodes = live_nodes;
        self.artifact_specs = artifact_specs;
        self.details_open
            .retain(|node_id, _| self.live_nodes.contains(node_id));
        initialize_details(&self.document.blocks, &mut self.details_open);
        self.diagram_source
            .retain(|node_id| self.live_nodes.contains(node_id));
        self.artifact_states
            .retain(|node_id, _| self.live_nodes.contains(node_id));
        self.highlight_cache
            .retain(|(node_id, _), _| self.live_nodes.contains(node_id));
        self.artifact_controller
            .prune(&self.view_id, self.document.revision, &self.live_nodes);
        self.rebuild_virtual_layout(AGENT_BLOCK_ESTIMATE_WIDTH_PX);
        self.rebuild_anchors();
    }

    fn should_virtualize_blocks(&self) -> bool {
        if self.options.presentation != MarkdownPresentation::Agent {
            return false;
        }
        let block_count = self.document.blocks.len();
        block_count >= AGENT_BLOCK_VIRTUALIZATION_MIN_BLOCKS
            || (self.document.source.len() >= AGENT_BLOCK_VIRTUALIZATION_MIN_SOURCE_BYTES
                && block_count >= AGENT_BLOCK_VIRTUALIZATION_MIN_LARGE_BLOCKS)
    }

    fn rebuild_virtual_layout(&mut self, width: f32) {
        let width = width.max(1.0);
        let chars_per_line = ((width / 7.0).floor() as usize).clamp(24, 120);
        let block_count = self.document.blocks.len();
        let mut sizes = Vec::with_capacity(block_count);
        let mut origins = Vec::with_capacity(block_count);
        let mut origin = px(0.0);
        for (index, block) in self.document.blocks.iter().enumerate() {
            origins.push(origin);
            let gap = if index + 1 == block_count {
                0.0
            } else {
                AGENT_BLOCK_GAP_PX
            };
            let height = estimated_markdown_block_height(
                block,
                self.document.source_for(block.range),
                chars_per_line,
            ) + gap;
            let height = px(height.ceil().max(1.0));
            sizes.push(height);
            origin += height;
        }
        self.virtual_block_sizes = Arc::new(sizes);
        self.virtual_block_origins = Arc::new(origins);
    }

    fn prepare_virtual_layout(&mut self, width: Pixels) -> bool {
        if self
            .virtual_layout_width
            .is_some_and(|current| f32::from(current - width).abs() < 1.0)
        {
            return false;
        }
        self.virtual_layout_width = Some(width);
        self.rebuild_virtual_layout(f32::from(width));
        true
    }

    fn virtual_total_height(&self) -> Pixels {
        self.virtual_block_origins
            .last()
            .zip(self.virtual_block_sizes.last())
            .map(|(origin, size)| *origin + *size)
            .unwrap_or_default()
    }

    fn visible_virtual_blocks(
        &self,
        flow_bounds: Bounds<Pixels>,
        viewport: Bounds<Pixels>,
    ) -> Range<usize> {
        let block_count = self.virtual_block_sizes.len();
        if block_count == 0 || flow_bounds.size.width <= px(0.0) {
            return 0..0;
        }
        let visible_top = viewport.top() - px(AGENT_BLOCK_VIRTUALIZATION_OVERSCAN_PX);
        let visible_bottom = viewport.bottom() + px(AGENT_BLOCK_VIRTUALIZATION_OVERSCAN_PX);
        let local_visible_top = visible_top - flow_bounds.top();
        let first = if local_visible_top < px(0.0) {
            0
        } else if local_visible_top >= self.virtual_total_height() {
            block_count
        } else {
            // Origins are cumulative. The block containing the top edge is the
            // predecessor of the first origin beyond it, so lookup stays
            // logarithmic even for documents with thousands of blocks.
            self.virtual_block_origins
                .partition_point(|origin| *origin <= local_visible_top)
                .saturating_sub(1)
        };
        let end = self.virtual_block_origins[first..]
            .partition_point(|origin| flow_bounds.top() + *origin < visible_bottom)
            .saturating_add(first)
            .min(block_count);
        first..end
    }

    fn prepare_virtual_selection(&mut self, visible_blocks: Range<usize>) {
        if self.virtual_visible_blocks.as_ref() != Some(&visible_blocks) {
            if !self.select_all_document {
                self.text_selection = MarkdownTextSelection::default();
            }
            self.virtual_visible_blocks = Some(visible_blocks);
        }
    }

    fn render_virtual_block(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let block = self.document.blocks.get(index)?.clone();
        let is_last = index + 1 == self.document.blocks.len();
        Some(
            div()
                .w_full()
                .min_w_0()
                .when(!is_last, |this| this.pb(px(AGENT_BLOCK_GAP_PX)))
                .child(self.render_block(&block, window, cx))
                .into_any_element(),
        )
    }

    fn record_virtual_block_heights(
        &mut self,
        width: Pixels,
        measurements: &[(usize, Pixels)],
    ) -> bool {
        if self
            .virtual_layout_width
            .is_none_or(|current| f32::from(current - width).abs() >= 1.0)
        {
            return false;
        }
        let sizes = Arc::make_mut(&mut self.virtual_block_sizes);
        let mut first_changed = None;
        for (index, measured) in measurements {
            let measured = px(f32::from(*measured).ceil().max(1.0));
            let Some(current) = sizes.get_mut(*index) else {
                continue;
            };
            if f32::from(*current - measured).abs() >= 1.0 {
                *current = measured;
                first_changed =
                    Some(first_changed.map_or(*index, |first: usize| first.min(*index)));
            }
        }
        let Some(first_changed) = first_changed else {
            return false;
        };

        let sizes = self.virtual_block_sizes.clone();
        let origins = Arc::make_mut(&mut self.virtual_block_origins);
        let first_changed = if origins.len() == sizes.len() {
            first_changed
        } else {
            0
        };
        origins.resize(sizes.len(), px(0.0));
        let mut origin = first_changed
            .checked_sub(1)
            .map(|previous| origins[previous] + sizes[previous])
            .unwrap_or_default();
        for index in first_changed..sizes.len() {
            origins[index] = origin;
            origin += sizes[index];
        }
        true
    }

    fn rebuild_anchors(&mut self) {
        self.anchors.clear();
        let Some(scroll) = self.options.scroll_handle.clone() else {
            return;
        };
        for entry in self.document.outline.iter() {
            self.anchors
                .insert(entry.node_id, ScrollAnchor::for_handle(scroll.clone()));
        }
        for node_id in self.document.footnotes.definitions.values() {
            self.anchors
                .insert(*node_id, ScrollAnchor::for_handle(scroll.clone()));
        }
        for node_id in self.document.footnotes.references.values().flatten() {
            self.anchors
                .insert(*node_id, ScrollAnchor::for_handle(scroll.clone()));
        }
    }

    fn ensure_artifacts(&mut self, window: &Window, cx: &mut Context<Self>) {
        let theme = if cx.theme().is_dark() {
            ArtifactTheme::Dark
        } else {
            ArtifactTheme::Light
        };
        let foreground_rgb = u32::from(cx.theme().foreground.to_rgb()) >> 8;
        let scale_factor = window.scale_factor();
        let specs = self
            .artifact_specs
            .iter()
            .map(|spec| (spec.node_id, spec.kind, spec.source.clone()))
            .collect::<Vec<_>>();
        for (node_id, kind, source) in specs {
            let request = ArtifactRequest {
                view_id: self.view_id.clone(),
                revision: self.document.revision,
                node_id,
                kind,
                source,
                theme,
                foreground_rgb,
                font_size: f32::from(cx.theme().mono_font_size),
                scale_factor,
            };
            let key = request.key();
            if self
                .artifact_states
                .get(&node_id)
                .is_some_and(|state| artifact_state_key(state) == key)
            {
                continue;
            }
            match self.artifact_controller.schedule(request.clone()) {
                ArtifactSchedule::Cached(artifact) => {
                    self.start_artifact(request, Some(artifact), cx)
                }
                ArtifactSchedule::Start(request) => self.start_artifact(request, None, cx),
                ArtifactSchedule::Queued => {
                    self.artifact_states
                        .insert(node_id, ArtifactDisplayState::Loading { key });
                }
                ArtifactSchedule::Existing => {
                    self.artifact_states
                        .insert(node_id, ArtifactDisplayState::Loading { key });
                }
                ArtifactSchedule::Rejected(error) => {
                    self.artifact_states.insert(
                        node_id,
                        ArtifactDisplayState::Failed {
                            key,
                            message: bounded_text(&error.to_string(), 240),
                        },
                    );
                }
            }
        }
    }

    fn start_artifact(
        &mut self,
        request: ArtifactRequest,
        cached: Option<Arc<SvgArtifact>>,
        cx: &mut Context<Self>,
    ) {
        let node_id = request.node_id;
        let key = request.key();
        let from_cache = cached.is_some();
        self.artifact_states
            .insert(node_id, ArtifactDisplayState::Loading { key });
        let svg_renderer = cx.svg_renderer();
        let timeout = self.artifact_controller.timeout();
        let requested_scale = if request.scale_factor.is_finite() {
            request.scale_factor.clamp(1.0, 4.0)
        } else {
            1.0
        };
        let worker_request = request.clone();
        let worker = cx.background_spawn(async move {
            let artifact = match cached {
                Some(artifact) => Ok(artifact),
                None => render_local_artifact_with_timeout(
                    worker_request,
                    SvgPolicy::default(),
                    timeout,
                ),
            }?;
            let intrinsic_pixels = artifact.width_px * artifact.height_px;
            let budget_scale = (ARTIFACT_MAX_RASTER_PIXELS / intrinsic_pixels.max(1.0)).sqrt();
            let scale = requested_scale.min(budget_scale).max(1.0);
            let image = svg_renderer
                .render_single_frame(&artifact.bytes, scale)
                .map_err(|error| ArtifactError::Engine(error.to_string()))?;
            Ok::<_, ArtifactError>((artifact, image))
        });
        self.artifact_tasks.insert(
            key,
            cx.spawn(async move |entity: WeakEntity<Self>, cx| {
                let outcome = worker.await;
                let _ = entity.update(cx, |this, cx| {
                    this.finish_artifact(request, from_cache, outcome, cx);
                });
            }),
        );
    }

    fn finish_artifact(
        &mut self,
        request: ArtifactRequest,
        from_cache: bool,
        outcome: Result<(Arc<SvgArtifact>, Arc<RenderImage>), ArtifactError>,
        cx: &mut Context<Self>,
    ) {
        self.artifact_tasks.remove(&request.key());
        let request_key = request.key();
        let accepted_by_document = request.view_id == self.view_id
            && request.revision == self.document.revision
            && self.live_nodes.contains(&request.node_id);
        let accepted = accepted_by_document
            && self
                .artifact_states
                .get(&request.node_id)
                .is_some_and(|state| artifact_state_key(state) == request_key);
        let next = if from_cache {
            None
        } else {
            let artifact_result = outcome
                .as_ref()
                .map(|(artifact, _)| artifact.clone())
                .map_err(Clone::clone);
            let completion = self.artifact_controller.complete(
                &request,
                artifact_result,
                &self.view_id,
                self.document.revision,
                &self.live_nodes,
            );
            debug_assert_eq!(completion.accepted, accepted_by_document);
            completion.next
        };
        if accepted {
            let key = request_key;
            match outcome {
                Ok((artifact, image)) => {
                    self.artifact_states.insert(
                        request.node_id,
                        ArtifactDisplayState::Ready {
                            key,
                            image,
                            artifact,
                        },
                    );
                }
                Err(error) => {
                    self.artifact_states.insert(
                        request.node_id,
                        ArtifactDisplayState::Failed {
                            key,
                            message: bounded_text(&error.to_string(), 240),
                        },
                    );
                }
            }
        }
        if let Some(next) = next {
            self.start_artifact(next, None, cx);
        }
        cx.notify();
    }

    fn toggle_details(&mut self, node_id: NodeId, cx: &mut Context<Self>) {
        let open = self.details_open.entry(node_id).or_default();
        *open = !*open;
        self.text_selection = MarkdownTextSelection::default();
        cx.notify();
    }

    fn toggle_diagram_source(&mut self, node_id: NodeId, cx: &mut Context<Self>) {
        if !self.diagram_source.remove(&node_id) {
            self.diagram_source.insert(node_id);
        }
        self.text_selection = MarkdownTextSelection::default();
        cx.notify();
    }

    fn toggle_outline(&mut self, cx: &mut Context<Self>) {
        self.outline_open = !self.outline_open;
        cx.notify();
    }

    fn begin_selection_frame(&mut self) {
        self.selection_frame = self.selection_frame.saturating_add(1).max(1);
        self.selection_next_segment = 0;
        self.selection_text.clear();
        self.selection_segments.clear();
    }

    fn selection_block_break(&mut self) {
        if !self.selection_text.is_empty() && !self.selection_text.ends_with('\n') {
            self.selection_text.push('\n');
        }
    }

    fn selection_inline_break(&mut self) {
        self.selection_text.push('\n');
    }

    fn push_selection_source(&mut self, source: &str) {
        self.selection_text.push_str(source);
    }

    fn selectable_styled_text(
        &mut self,
        text: impl Into<SharedString>,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
        cx: &mut Context<Self>,
    ) -> MarkdownSelectableText {
        self.selectable_interactive_text(text, highlights, Vec::new(), Vec::new(), cx)
    }

    fn selectable_interactive_text(
        &mut self,
        text: impl Into<SharedString>,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
        mono_ranges: Vec<Range<usize>>,
        actions: Vec<(Range<usize>, MarkdownInlineClickHandler)>,
        cx: &mut Context<Self>,
    ) -> MarkdownSelectableText {
        let text = text.into();
        let start = self.selection_text.len();
        self.selection_text.push_str(&text);
        let text_range = start..self.selection_text.len();
        let selection = self.text_selection.range();
        let selection_start = selection.start.max(text_range.start);
        let selection_end = selection.end.min(text_range.end);
        let highlights = if selection_start < selection_end {
            overlay_selection_highlight(
                text.len(),
                highlights,
                selection_start - text_range.start..selection_end - text_range.start,
                cx.theme().selection,
            )
        } else {
            highlights
        };
        let segment = self.selection_next_segment;
        self.selection_next_segment = self.selection_next_segment.saturating_add(1);
        MarkdownSelectableText::new(
            format!("markdown-text:{}:{segment}", self.view_id),
            cx.entity().downgrade(),
            self.selection_frame,
            segment,
            text_range,
            text,
            MarkdownSelectableTextDecorations {
                highlights,
                mono_ranges,
                actions,
                mono_font_family: cx.theme().mono_font_family.clone(),
            },
        )
    }

    fn register_selection_segment(
        &mut self,
        frame: u64,
        segment: usize,
        text_range: Range<usize>,
        bounds: Bounds<Pixels>,
        layout: TextLayout,
    ) {
        if frame != self.selection_frame || text_range.is_empty() {
            return;
        }
        self.selection_segments.insert(
            segment,
            SelectionSegment {
                frame,
                text_range,
                bounds,
                layout,
            },
        );
    }

    fn normalize_text_selection(&mut self) {
        if self.selection_text.is_empty() {
            self.text_selection = MarkdownTextSelection::default();
            return;
        }
        if self.select_all_document {
            self.text_selection = MarkdownTextSelection {
                anchor: 0,
                head: self.selection_text.len(),
                pending: false,
            };
            return;
        }
        self.text_selection.anchor = text_boundary_at_or_before(
            &self.selection_text,
            self.text_selection.anchor.min(self.selection_text.len()),
        );
        self.text_selection.head = text_boundary_at_or_before(
            &self.selection_text,
            self.text_selection.head.min(self.selection_text.len()),
        );
    }

    fn exact_selection_index(&self, position: Point<Pixels>) -> Option<usize> {
        self.selection_segments
            .values()
            .filter(|segment| {
                segment.frame == self.selection_frame && segment.bounds.contains(&position)
            })
            .min_by(|left, right| {
                selection_bounds_area(left.bounds).total_cmp(&selection_bounds_area(right.bounds))
            })
            .map(|segment| self.selection_index_in_segment(segment, position))
    }

    fn nearest_selection_index(&self, position: Point<Pixels>) -> Option<usize> {
        self.selection_segments
            .values()
            .filter(|segment| segment.frame == self.selection_frame)
            .min_by(|left, right| {
                selection_bounds_distance(left.bounds, position)
                    .total_cmp(&selection_bounds_distance(right.bounds, position))
            })
            .map(|segment| self.selection_index_in_segment(segment, position))
    }

    fn selection_index_in_segment(
        &self,
        segment: &SelectionSegment,
        position: Point<Pixels>,
    ) -> usize {
        let local = match segment.layout.index_for_position(position) {
            Ok(index) | Err(index) => index,
        }
        .min(segment.text_range.len());
        text_boundary_at_or_before(
            &self.selection_text,
            segment.text_range.start.saturating_add(local),
        )
    }

    fn start_text_selection(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.exact_selection_index(event.position) else {
            self.clear_text_selection(cx);
            return;
        };
        self.select_all_document = false;
        let range = match event.click_count {
            2 => selection_word_range(&self.selection_text, index),
            3 => selection_line_range(&self.selection_text, index),
            count if count >= 4 => 0..self.selection_text.len(),
            _ => index..index,
        };
        if event.click_count == 1 && event.modifiers.shift {
            self.text_selection.head = index;
            self.text_selection.pending = true;
        } else {
            self.text_selection = MarkdownTextSelection {
                anchor: range.start,
                head: range.end,
                pending: true,
            };
        }
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn update_text_selection(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.text_selection.pending || !event.dragging() {
            return;
        }
        let Some(head) = self.nearest_selection_index(event.position) else {
            return;
        };
        if self.text_selection.head != head {
            self.text_selection.head = head;
            if !self.text_selection.range().is_empty() {
                window.prevent_default();
            }
            cx.notify();
        }
    }

    fn finish_text_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.text_selection.pending {
            return;
        }
        self.text_selection.pending = false;
        let selected = self.selected_text();
        if !selected.is_empty() {
            window.prevent_default();
            #[cfg(any(target_os = "linux", target_os = "freebsd"))]
            cx.write_to_primary(ClipboardItem::new_string(selected));
        }
        cx.notify();
    }

    fn clear_text_selection(&mut self, cx: &mut Context<Self>) {
        if self.text_selection == MarkdownTextSelection::default() && !self.select_all_document {
            return;
        }
        self.text_selection = MarkdownTextSelection::default();
        self.select_all_document = false;
        cx.notify();
    }

    fn selected_text(&self) -> String {
        if self.select_all_document {
            return self.document.plain_text();
        }
        let range = self.text_selection.range();
        self.selection_text
            .get(range)
            .unwrap_or_default()
            .to_string()
    }

    fn copy_text_selection(&mut self, _: &CopyAction, _: &mut Window, cx: &mut Context<Self>) {
        let selected = self.selected_text();
        if selected.is_empty() {
            cx.propagate();
        } else {
            cx.write_to_clipboard(ClipboardItem::new_string(selected));
        }
    }

    fn select_all_text(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        if self.selection_text.is_empty() && !self.virtualized_selection {
            cx.propagate();
            return;
        }
        self.select_all_document = self.virtualized_selection;
        self.text_selection = MarkdownTextSelection {
            anchor: 0,
            head: self.selection_text.len(),
            pending: false,
        };
        cx.notify();
    }
}

struct MarkdownSelectableText {
    element_id: ElementId,
    owner: WeakEntity<MarkdownViewState>,
    frame: u64,
    segment: usize,
    text_range: Range<usize>,
    interactive_text: InteractiveText,
    layout: TextLayout,
}

impl MarkdownSelectableText {
    fn new(
        element_id: impl Into<ElementId>,
        owner: WeakEntity<MarkdownViewState>,
        frame: u64,
        segment: usize,
        text_range: Range<usize>,
        text: SharedString,
        decorations: MarkdownSelectableTextDecorations,
    ) -> Self {
        let element_id = element_id.into();
        let MarkdownSelectableTextDecorations {
            highlights,
            mono_ranges,
            actions,
            mono_font_family,
        } = decorations;
        let mut styled_text = if highlights.is_empty() {
            StyledText::new(text)
        } else {
            StyledText::new(text).with_highlights(highlights)
        };
        if !mono_ranges.is_empty() {
            styled_text = styled_text.with_font_family_overrides(
                mono_ranges
                    .into_iter()
                    .map(|range| (range, mono_font_family.clone())),
            );
        }
        let layout = styled_text.layout().clone();
        let action_ranges = actions
            .iter()
            .map(|(range, _)| range.clone())
            .collect::<Vec<_>>();
        let handlers = actions
            .into_iter()
            .map(|(_, handler)| handler)
            .collect::<Vec<_>>();
        let interactive_text = if action_ranges.is_empty() {
            InteractiveText::new(element_id.clone(), styled_text)
        } else {
            InteractiveText::new(element_id.clone(), styled_text).on_click(
                action_ranges,
                move |index, window, cx| {
                    if let Some(handler) = handlers.get(index) {
                        handler(window, cx);
                    }
                },
            )
        };
        Self {
            element_id,
            owner,
            frame,
            segment,
            text_range,
            interactive_text,
            layout,
        }
    }
}

impl IntoElement for MarkdownSelectableText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for MarkdownSelectableText {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.element_id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.interactive_text
            .request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let hitbox =
            self.interactive_text
                .prepaint(id, inspector_id, bounds, request_layout, window, cx);
        let _ = self.owner.update(cx, |state, _| {
            state.register_selection_segment(
                self.frame,
                self.segment,
                self.text_range.clone(),
                bounds,
                self.layout.clone(),
            );
        });
        hitbox
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.interactive_text.paint(
            id,
            inspector_id,
            bounds,
            request_layout,
            prepaint,
            window,
            cx,
        );
    }
}

fn overlay_selection_highlight(
    text_len: usize,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    selection: Range<usize>,
    selection_background: ::gpui::Hsla,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let selection = selection.start.min(text_len)..selection.end.min(text_len);
    let selection_style = HighlightStyle {
        background_color: Some(selection_background),
        ..Default::default()
    };
    let highlights = combine_highlights(std::iter::empty(), highlights).collect::<Vec<_>>();
    let mut boundaries = vec![selection.start, selection.end];
    for (range, _) in &highlights {
        boundaries.extend([range.start, range.end]);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut highlight_index = 0;
    boundaries
        .windows(2)
        .filter_map(move |bounds| {
            let range = bounds[0]..bounds[1];
            if range.is_empty() {
                return None;
            }
            while highlights
                .get(highlight_index)
                .is_some_and(|(highlight_range, _)| highlight_range.end <= range.start)
            {
                highlight_index += 1;
            }
            let semantic = highlights
                .get(highlight_index)
                .filter(|(highlight_range, _)| {
                    highlight_range.start <= range.start && highlight_range.end >= range.end
                })
                .map(|(_, style)| *style);
            let selected = selection.start <= range.start && selection.end >= range.end;
            match (semantic, selected) {
                (Some(style), true) => Some((range, style.highlight(selection_style))),
                (Some(style), false) => Some((range, style)),
                (None, true) => Some((range, selection_style)),
                (None, false) => None,
            }
        })
        .collect()
}

fn text_boundary_at_or_before(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn selection_word_range(text: &str, index: usize) -> Range<usize> {
    if text.is_empty() {
        return 0..0;
    }
    let mut index = text_boundary_at_or_before(text, index);
    if index == text.len() {
        index = text
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
            .unwrap_or_default();
    }
    let selected = text[index..].chars().next().unwrap_or(' ');
    let is_word = |character: char| character.is_alphanumeric() || character == '_';
    let class = is_word(selected);
    let start = text[..index]
        .char_indices()
        .rev()
        .take_while(|(_, character)| is_word(*character) == class && *character != '\n')
        .last()
        .map(|(index, _)| index)
        .unwrap_or(index);
    let end = text[index..]
        .char_indices()
        .skip(1)
        .find(|(_, character)| is_word(*character) != class || *character == '\n')
        .map(|(offset, _)| index + offset)
        .unwrap_or(text.len());
    start..end
}

fn selection_line_range(text: &str, index: usize) -> Range<usize> {
    let index = text_boundary_at_or_before(text, index);
    let start = text[..index].rfind('\n').map_or(0, |offset| offset + 1);
    let end = text[index..]
        .find('\n')
        .map_or(text.len(), |offset| index + offset);
    start..end
}

fn selection_bounds_area(bounds: Bounds<Pixels>) -> f32 {
    f32::from(bounds.size.width).max(0.0) * f32::from(bounds.size.height).max(0.0)
}

fn selection_bounds_distance(bounds: Bounds<Pixels>, position: Point<Pixels>) -> f32 {
    let x = if position.x < bounds.left() {
        f32::from(bounds.left() - position.x)
    } else if position.x > bounds.right() {
        f32::from(position.x - bounds.right())
    } else {
        0.0
    };
    let y = if position.y < bounds.top() {
        f32::from(bounds.top() - position.y)
    } else if position.y > bounds.bottom() {
        f32::from(position.y - bounds.bottom())
    } else {
        0.0
    };
    x.mul_add(x, y * y)
}

fn image_alt_text(alt: &str) -> String {
    if alt.is_empty() {
        "Image".to_string()
    } else {
        alt.to_string()
    }
}

fn estimated_markdown_block_height(block: &BlockNode, source: &str, chars_per_line: usize) -> f32 {
    let wrapped_lines = source
        .lines()
        .map(|line| line.chars().count().max(1).div_ceil(chars_per_line.max(1)))
        .sum::<usize>()
        .max(1) as f32;
    match &block.kind {
        Block::Heading { level, .. } => match level {
            1 => 32.0,
            2 => 29.0,
            3 => 26.0,
            4 => 24.0,
            _ => 22.0,
        },
        Block::Code { source, .. } | Block::Literal(source) => {
            source.lines().count().max(1) as f32 * 20.0 + 42.0
        }
        Block::Diff { source } => source.lines().count().max(1) as f32 * 20.0 + 30.0,
        Block::Math { .. } | Block::Diagram { .. } => 220.0,
        Block::Table { header, rows, .. } => {
            (rows.len() + usize::from(header.is_some())).max(1) as f32 * 36.0 + 2.0
        }
        Block::Image(_) => 220.0,
        Block::ThematicBreak => 8.0,
        Block::Progress { .. } => 28.0,
        _ => wrapped_lines * 22.0,
    }
    .max(1.0)
}

impl Render for MarkdownViewState {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_artifacts(window, cx);
        self.begin_selection_frame();
        let virtualized = self.should_virtualize_blocks();
        self.virtualized_selection = virtualized;
        if !virtualized {
            self.virtual_visible_blocks = None;
        }
        let focus_handle = self.focus_handle.clone();
        let mut root = v_flex()
            .id(format!("markdown-root:{}", self.view_id))
            .key_context("TextView")
            .track_focus(&focus_handle)
            .on_action(cx.listener(Self::copy_text_selection))
            .on_action(cx.listener(Self::select_all_text))
            .w_full()
            .min_w_0()
            .when(!virtualized, |this| this.gap_3())
            .text_sm()
            .line_height(px(22.0))
            .text_color(match self.options.presentation {
                MarkdownPresentation::Thought => cx.theme().muted_foreground,
                _ => cx.theme().foreground,
            });
        if self.options.presentation == MarkdownPresentation::Document {
            let outline_open = self.outline_open;
            let copy = self.document.plain_text();
            root = root
                .child(
                    h_flex()
                        .h(px(28.0))
                        .w_full()
                        .flex_none()
                        .justify_end()
                        .gap_1()
                        .when(!self.document.outline.is_empty(), |this| {
                            this.child(
                                Button::new(format!("markdown-outline:{}", self.view_id))
                                    .xsmall()
                                    .ghost()
                                    .compact()
                                    .selected(outline_open)
                                    .icon(if outline_open {
                                        IconName::ChevronUp
                                    } else {
                                        IconName::ChevronDown
                                    })
                                    .label("Contents")
                                    .tooltip("Toggle document outline")
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.toggle_outline(cx)),
                                    ),
                            )
                        })
                        .child(
                            Button::new(format!("markdown-copy-document:{}", self.view_id))
                                .xsmall()
                                .ghost()
                                .compact()
                                .icon(IconName::Copy)
                                .tooltip("Copy document text")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                                }),
                        ),
                )
                .when(outline_open, |this| this.child(self.render_toc(cx)));
        }
        if virtualized {
            root = root.child(MarkdownVirtualFlow::new(
                cx.entity().clone(),
                self.virtual_total_height(),
            ));
        } else {
            let blocks = self.document.blocks.clone();
            for block in blocks.iter() {
                root = root.child(self.render_block(block, window, cx));
            }
            self.normalize_text_selection();
        }
        root
    }
}

impl MarkdownViewState {
    fn render_block(
        &mut self,
        block: &BlockNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.selection_block_break();
        match &block.kind {
            Block::Paragraph(inlines) => self.render_inlines(inlines, window, cx),
            Block::Heading {
                level,
                slug,
                content,
            } => {
                let anchor = self.anchors.get(&block.id).cloned();
                let size = match level {
                    1 => 24.0,
                    2 => 21.0,
                    3 => 18.0,
                    4 => 16.0,
                    5 => 15.0,
                    _ => 14.0,
                };
                div()
                    .id(format!("markdown-heading:{slug}"))
                    .w_full()
                    .min_w_0()
                    .anchor_scroll(anchor)
                    .text_size(px(size))
                    .font_weight(FontWeight::SEMIBOLD)
                    .line_height(px(size + 8.0))
                    .child(self.render_inlines(content, window, cx))
                    .into_any_element()
            }
            Block::Quote(children) => v_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .pl_3()
                .border_l_2()
                .border_color(cx.theme().border)
                .text_color(cx.theme().muted_foreground)
                .children(
                    children
                        .iter()
                        .map(|child| self.render_block(child, window, cx)),
                )
                .into_any_element(),
            Block::Callout {
                kind,
                title,
                children,
            } => {
                let accent = match kind {
                    CalloutKind::Note | CalloutKind::Important => cx.theme().info,
                    CalloutKind::Tip => cx.theme().success,
                    CalloutKind::Warning => cx.theme().warning,
                    CalloutKind::Caution => cx.theme().danger,
                };
                let title = self.selectable_styled_text(title.clone(), Vec::new(), cx);
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .p_3()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(accent.opacity(0.55))
                    .bg(accent.opacity(if cx.theme().is_dark() { 0.12 } else { 0.07 }))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(accent)
                            .child(title),
                    )
                    .children(
                        children
                            .iter()
                            .map(|child| self.render_block(child, window, cx)),
                    )
                    .into_any_element()
            }
            Block::Code {
                language, source, ..
            } => self.render_code(block.id, language.as_deref(), source, cx),
            Block::Diff { source } => self.render_diff(block.id, source, cx),
            Block::Math { source } => {
                self.render_artifact(block.id, ArtifactKind::DisplayMath, source, cx)
            }
            Block::Diagram { kind, source } => self.render_artifact(
                block.id,
                match kind {
                    DiagramKind::Mermaid => ArtifactKind::Mermaid,
                    DiagramKind::PlantUml => ArtifactKind::PlantUml,
                },
                source,
                cx,
            ),
            Block::List { start, items } => {
                let task_count = items.iter().filter(|item| item.checked.is_some()).count();
                let complete = items
                    .iter()
                    .filter(|item| item.checked == Some(true))
                    .count();
                let mut list = v_flex().w_full().min_w_0().gap_2();
                if task_count > 0 {
                    list = list.child(self.render_progress(
                        block.id,
                        complete as f64,
                        task_count as f64,
                        Some(format!("{complete} / {task_count}")),
                        cx,
                    ));
                }
                for (index, item) in items.iter().enumerate() {
                    let marker = start
                        .map(|start| format!("{}.", start + index as u64))
                        .unwrap_or_else(|| "•".into());
                    let leading = if let Some(checked) = item.checked {
                        Checkbox::new(format!("markdown-task:{}", item.id.0))
                            .checked(checked)
                            .disabled(true)
                            .tab_stop(false)
                            .into_any_element()
                    } else {
                        div()
                            .w(px(22.0))
                            .flex_none()
                            .text_color(cx.theme().muted_foreground)
                            .child(marker)
                            .into_any_element()
                    };
                    list = list.child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .items_start()
                            .gap_2()
                            .child(leading)
                            .child(
                                v_flex().flex_1().min_w_0().gap_2().children(
                                    item.children
                                        .iter()
                                        .map(|child| self.render_block(child, window, cx)),
                                ),
                            ),
                    );
                }
                list.into_any_element()
            }
            Block::DefinitionList(items) => v_flex()
                .w_full()
                .min_w_0()
                .gap_3()
                .children(items.iter().map(|item| {
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap_1()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(self.render_inlines(&item.term, window, cx)),
                        )
                        .children(item.definitions.iter().map(|definition| {
                            v_flex().min_w_0().pl_4().gap_2().children(
                                definition
                                    .iter()
                                    .map(|child| self.render_block(child, window, cx)),
                            )
                        }))
                }))
                .into_any_element(),
            Block::Table {
                alignments,
                header,
                rows,
            } => self.render_table(alignments, header.as_ref(), rows, window, cx),
            Block::ThematicBreak => div()
                .w_full()
                .h(px(1.0))
                .bg(cx.theme().border)
                .into_any_element(),
            Block::TableOfContents => self.render_toc(cx),
            Block::Details {
                summary, children, ..
            } => {
                let open = self.details_open.get(&block.id).copied().unwrap_or(false);
                let node_id = block.id;
                v_flex()
                    .w_full()
                    .min_w_0()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_2()
                            .p_2()
                            .child(
                                Button::new(format!("markdown-details:{}", block.id.0))
                                    .xsmall()
                                    .ghost()
                                    .compact()
                                    .selected(open)
                                    .icon(if open {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .tooltip(if open { "Collapse" } else { "Expand" })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.toggle_details(node_id, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(self.render_inlines(summary, window, cx)),
                            ),
                    )
                    .when(open, |this| {
                        this.child(
                            v_flex().w_full().min_w_0().gap_2().px_3().pb_3().children(
                                children
                                    .iter()
                                    .map(|child| self.render_block(child, window, cx)),
                            ),
                        )
                    })
                    .into_any_element()
            }
            Block::Progress { value, max, label } => {
                self.render_progress(block.id, *value, *max, label.clone(), cx)
            }
            Block::Image(image) => self.render_image(image, false, cx),
            Block::FootnoteDefinition { label, children } => {
                let anchor = self.anchors.get(&block.id).cloned();
                let back_anchor = self
                    .document
                    .footnotes
                    .references
                    .get(label)
                    .and_then(|references| references.first())
                    .and_then(|node_id| self.anchors.get(node_id))
                    .cloned();
                let label_text = self.selectable_styled_text(
                    SharedString::from(format!("[{label}]")),
                    Vec::new(),
                    cx,
                );
                v_flex()
                    .id(format!("markdown-footnote:{label}"))
                    .anchor_scroll(anchor)
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .pl_3()
                    .border_l_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().muted_foreground)
                            .child(label_text)
                            .when_some(back_anchor, |this, anchor| {
                                this.child(
                                    Link::new(format!(
                                        "markdown-footnote-back:{}:{label}",
                                        block.id.0
                                    ))
                                    .on_click(move |_, window, cx| anchor.scroll_to(window, cx))
                                    .child("Back to reference"),
                                )
                            }),
                    )
                    .children(
                        children
                            .iter()
                            .map(|child| self.render_block(child, window, cx)),
                    )
                    .into_any_element()
            }
            Block::SafeHtml(children) => v_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .children(
                    children
                        .iter()
                        .map(|child| self.render_block(child, window, cx)),
                )
                .into_any_element(),
            Block::Literal(source) => self.render_code(block.id, None, source, cx),
        }
    }

    fn render_inlines(
        &mut self,
        inlines: &[InlineNode],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if inlines.iter().all(inline_supports_text_flow) {
            let mut flow = InlineTextFlow::default();
            for inline in inlines {
                self.append_inline_text_flow(inline, InlineSemanticStyle::default(), &mut flow, cx);
            }
            let highlights = combine_highlights(
                std::iter::empty::<(Range<usize>, HighlightStyle)>(),
                flow.highlights,
            )
            .collect::<Vec<_>>();
            let text = self.selectable_interactive_text(
                flow.text,
                highlights,
                merge_inline_ranges(flow.mono_ranges),
                flow.actions,
                cx,
            );
            return div()
                .w_full()
                .min_w_0()
                .whitespace_normal()
                .child(text)
                .into_any_element();
        }

        h_flex()
            .w_full()
            .min_w_0()
            .flex_wrap()
            .items_center()
            .gap_y_1()
            .children(
                inlines
                    .iter()
                    .map(|inline| self.render_inline(inline, window, cx)),
            )
            .into_any_element()
    }

    fn append_inline_text_flow(
        &self,
        inline: &InlineNode,
        semantic: InlineSemanticStyle,
        flow: &mut InlineTextFlow,
        cx: &App,
    ) {
        match &inline.kind {
            Inline::Text(text) | Inline::Literal(text) => {
                self.push_inline_text(flow, text, semantic, None, false, cx)
            }
            Inline::Emphasis(children) => {
                for child in children {
                    self.append_inline_text_flow(child, semantic.emphasis(), flow, cx);
                }
            }
            Inline::Strong(children) => {
                for child in children {
                    self.append_inline_text_flow(child, semantic.strong(), flow, cx);
                }
            }
            Inline::Deletion(children) => {
                for child in children {
                    self.append_inline_text_flow(child, semantic.deletion(), flow, cx);
                }
            }
            Inline::Underline(children) => {
                for child in children {
                    self.append_inline_text_flow(child, semantic.underline(), flow, cx);
                }
            }
            Inline::Code(code) => self.push_inline_text(
                flow,
                code,
                semantic,
                Some(HighlightStyle {
                    background_color: Some(cx.theme().muted),
                    ..Default::default()
                }),
                true,
                cx,
            ),
            Inline::Link {
                destination,
                children,
                ..
            } => {
                let start = flow.text.len();
                for child in children {
                    self.append_inline_text_flow(child, semantic, flow, cx);
                }
                let range = start..flow.text.len();
                if range.is_empty() {
                    return;
                }
                let action = self.inline_text_link_action(destination);
                flow.highlights.push((
                    range.clone(),
                    HighlightStyle {
                        color: Some(if action.is_some() {
                            cx.theme().primary
                        } else {
                            cx.theme().muted_foreground
                        }),
                        underline: Some(UnderlineStyle {
                            thickness: px(1.0),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ));
                if let Some(action) = action {
                    flow.actions.push((range, action));
                }
            }
            Inline::Keycap(children) => {
                let start = flow.text.len();
                for child in children {
                    self.append_inline_text_flow(child, semantic, flow, cx);
                }
                let range = start..flow.text.len();
                if !range.is_empty() {
                    flow.highlights.push((
                        range.clone(),
                        HighlightStyle {
                            background_color: Some(cx.theme().muted),
                            ..Default::default()
                        },
                    ));
                    flow.mono_ranges.push(range);
                }
            }
            Inline::Mark(children) => {
                let start = flow.text.len();
                for child in children {
                    self.append_inline_text_flow(child, semantic, flow, cx);
                }
                let range = start..flow.text.len();
                if !range.is_empty() {
                    flow.highlights.push((
                        range,
                        HighlightStyle {
                            background_color: Some(
                                cx.theme().warning.opacity(if cx.theme().is_dark() {
                                    0.28
                                } else {
                                    0.22
                                }),
                            ),
                            ..Default::default()
                        },
                    ));
                }
            }
            Inline::Break => flow.text.push('\n'),
            Inline::Image(_)
            | Inline::Math(_)
            | Inline::Superscript(_)
            | Inline::Subscript(_)
            | Inline::FootnoteReference(_) => {
                unreachable!("element-flow-only inline nodes are filtered before collection")
            }
        }
    }

    fn push_inline_text(
        &self,
        flow: &mut InlineTextFlow,
        text: &str,
        semantic: InlineSemanticStyle,
        extra: Option<HighlightStyle>,
        mono: bool,
        cx: &App,
    ) {
        if text.is_empty() {
            return;
        }
        let start = flow.text.len();
        flow.text.push_str(text);
        let range = start..flow.text.len();
        let mut highlights = combine_inline_highlights(
            text.len(),
            semantic,
            search_highlights(text, self.options.search_query.as_deref(), cx),
        );
        if let Some(extra) = extra {
            highlights = combine_highlights(highlights, [(0..text.len(), extra)]).collect();
        }
        flow.highlights.extend(
            highlights
                .into_iter()
                .map(|(local, style)| (start + local.start..start + local.end, style)),
        );
        if mono {
            flow.mono_ranges.push(range);
        }
    }

    fn inline_text_link_action(
        &self,
        resource: &ResolvedResource,
    ) -> Option<MarkdownInlineClickHandler> {
        match resource.kind {
            ResourceKind::Http | ResourceKind::Workspace => {
                let handler = self.options.on_open_resource.clone()?;
                let resource = resource.clone();
                Some(Rc::new(move |window, cx| {
                    handler(resource.clone(), window, cx)
                }))
            }
            ResourceKind::Fragment => {
                let anchor = resource
                    .resolved
                    .as_deref()
                    .and_then(|fragment| fragment.strip_prefix('#'))
                    .and_then(|slug| {
                        self.document
                            .outline
                            .iter()
                            .find(|entry| entry.slug == slug)
                    })
                    .and_then(|entry| self.anchors.get(&entry.node_id))
                    .cloned()?;
                Some(Rc::new(move |window, cx| anchor.scroll_to(window, cx)))
            }
            ResourceKind::Blocked | ResourceKind::DataImage => None,
        }
    }

    fn render_inline(
        &mut self,
        inline: &InlineNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_inline_with_style(inline, InlineSemanticStyle::default(), window, cx)
    }

    fn render_inline_with_style(
        &mut self,
        inline: &InlineNode,
        semantic: InlineSemanticStyle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match &inline.kind {
            Inline::Text(text) | Inline::Literal(text) => {
                let highlights = combine_inline_highlights(
                    text.len(),
                    semantic,
                    search_highlights(text, self.options.search_query.as_deref(), cx),
                );
                let text = self.selectable_styled_text(text.clone(), highlights, cx);
                div().min_w_0().child(text).into_any_element()
            }
            Inline::Emphasis(children) => {
                let semantic = semantic.emphasis();
                h_flex()
                    .min_w_0()
                    .flex_wrap()
                    .italic()
                    .children(
                        children.iter().map(|child| {
                            self.render_inline_with_style(child, semantic, window, cx)
                        }),
                    )
                    .into_any_element()
            }
            Inline::Strong(children) => {
                let semantic = semantic.strong();
                h_flex()
                    .min_w_0()
                    .flex_wrap()
                    .font_weight(FontWeight::BOLD)
                    .children(
                        children.iter().map(|child| {
                            self.render_inline_with_style(child, semantic, window, cx)
                        }),
                    )
                    .into_any_element()
            }
            Inline::Deletion(children) => {
                let semantic = semantic.deletion();
                h_flex()
                    .min_w_0()
                    .flex_wrap()
                    .line_through()
                    .children(
                        children.iter().map(|child| {
                            self.render_inline_with_style(child, semantic, window, cx)
                        }),
                    )
                    .into_any_element()
            }
            Inline::Underline(children) => {
                let semantic = semantic.underline();
                h_flex()
                    .min_w_0()
                    .flex_wrap()
                    .underline()
                    .children(
                        children.iter().map(|child| {
                            self.render_inline_with_style(child, semantic, window, cx)
                        }),
                    )
                    .into_any_element()
            }
            Inline::Superscript(children) => h_flex()
                .min_w_0()
                .text_xs()
                .children(
                    children
                        .iter()
                        .map(|child| self.render_inline_with_style(child, semantic, window, cx)),
                )
                .into_any_element(),
            Inline::Subscript(children) => h_flex()
                .min_w_0()
                .text_xs()
                .children(
                    children
                        .iter()
                        .map(|child| self.render_inline_with_style(child, semantic, window, cx)),
                )
                .into_any_element(),
            Inline::Code(code) => {
                let highlights = combine_inline_highlights(code.len(), semantic, Vec::new());
                let text = self.selectable_styled_text(code.clone(), highlights, cx);
                div()
                    .min_w_0()
                    .px_1()
                    .rounded(px(4.0))
                    .bg(cx.theme().muted)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(cx.theme().mono_font_size)
                    .child(text)
                    .into_any_element()
            }
            Inline::Link {
                destination,
                children,
                ..
            } => self.render_link(inline.id, destination, children, semantic, window, cx),
            Inline::Image(image) => self.render_image(image, true, cx),
            Inline::Math(source) => {
                self.render_artifact(inline.id, ArtifactKind::InlineMath, source, cx)
            }
            Inline::Keycap(children) => h_flex()
                .min_w_0()
                .px_1()
                .h(px(22.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().background)
                .font_family(cx.theme().mono_font_family.clone())
                .text_xs()
                .shadow_xs()
                .children(
                    children
                        .iter()
                        .map(|child| self.render_inline_with_style(child, semantic, window, cx)),
                )
                .into_any_element(),
            Inline::Mark(children) => h_flex()
                .min_w_0()
                .px_0p5()
                .rounded(px(2.0))
                .bg(cx
                    .theme()
                    .warning
                    .opacity(if cx.theme().is_dark() { 0.28 } else { 0.22 }))
                .children(
                    children
                        .iter()
                        .map(|child| self.render_inline_with_style(child, semantic, window, cx)),
                )
                .into_any_element(),
            Inline::Break => {
                self.selection_inline_break();
                div().w_full().h_0().flex_none().into_any_element()
            }
            Inline::FootnoteReference(label) => {
                let reference_anchor = self.anchors.get(&inline.id).cloned();
                let anchor = self
                    .document
                    .footnotes
                    .definitions
                    .get(label)
                    .and_then(|node_id| self.anchors.get(node_id))
                    .cloned();
                let disabled = anchor.is_none();
                let label_text = self.selectable_styled_text(
                    SharedString::from(format!("[{label}]")),
                    combine_inline_highlights(label.len() + 2, semantic, Vec::new()),
                    cx,
                );
                div()
                    .id(format!("markdown-footnote-anchor:{}", inline.id.0))
                    .anchor_scroll(reference_anchor)
                    .child(
                        Link::new(format!("markdown-footnote-ref:{}:{}", inline.id.0, label))
                            .disabled(disabled)
                            .on_click(move |_, window, cx| {
                                if let Some(anchor) = &anchor {
                                    anchor.scroll_to(window, cx);
                                }
                            })
                            .child(label_text),
                    )
                    .into_any_element()
            }
        }
    }

    fn render_link(
        &mut self,
        node_id: NodeId,
        resource: &ResolvedResource,
        children: &[InlineNode],
        semantic: InlineSemanticStyle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let content = children
            .iter()
            .map(|child| self.render_inline_with_style(child, semantic, window, cx))
            .collect::<Vec<_>>();
        let mut link = Link::new(format!("markdown-link:{}", node_id.0));
        match resource.kind {
            ResourceKind::Http => {
                if let Some(handler) = self.options.on_open_resource.clone() {
                    let resource = resource.clone();
                    link =
                        link.on_click(move |_, window, cx| handler(resource.clone(), window, cx));
                } else {
                    link = link.disabled(true);
                }
            }
            ResourceKind::Workspace => {
                if let Some(handler) = self.options.on_open_resource.clone() {
                    let resource = resource.clone();
                    link =
                        link.on_click(move |_, window, cx| handler(resource.clone(), window, cx));
                } else {
                    link = link.disabled(true);
                }
            }
            ResourceKind::Fragment => {
                let anchor = resource
                    .resolved
                    .as_deref()
                    .and_then(|fragment| fragment.strip_prefix('#'))
                    .and_then(|slug| {
                        self.document
                            .outline
                            .iter()
                            .find(|entry| entry.slug == slug)
                    })
                    .and_then(|entry| self.anchors.get(&entry.node_id))
                    .cloned();
                if anchor.is_some() {
                    link = link.on_click(move |_, window, cx| {
                        if let Some(anchor) = &anchor {
                            anchor.scroll_to(window, cx);
                        }
                    });
                } else {
                    link = link.disabled(true);
                }
            }
            ResourceKind::Blocked | ResourceKind::DataImage => link = link.disabled(true),
        }
        link.children(content).into_any_element()
    }

    fn render_image(
        &mut self,
        image: &InlineImage,
        inline: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let resource = &image.destination;
        let alt_text = image_alt_text(&image.alt);
        let loaded = resource
            .resolved
            .as_ref()
            .and_then(|path| self.options.images.get(path))
            .cloned()
            .or_else(|| self.options.images.get(&resource.source).cloned())
            .or_else(|| self.data_image(resource));
        if let Some(loaded_image) = loaded {
            self.push_selection_source(&alt_text);
            return img(loaded_image)
                .max_w_full()
                .max_h(px(if inline { 220.0 } else { 520.0 }))
                .object_fit(ObjectFit::Contain)
                .into_any_element();
        }
        if self.options.allow_http_images
            && resource.kind == ResourceKind::Http
            && let Some(url) = resource.resolved.clone()
        {
            self.push_selection_source(&alt_text);
            return img(url)
                .max_w_full()
                .max_h(px(if inline { 220.0 } else { 520.0 }))
                .object_fit(ObjectFit::Contain)
                .into_any_element();
        }
        let fallback = if image.alt.is_empty() {
            "Image unavailable".to_string()
        } else {
            image.alt.clone()
        };
        let fallback = self.selectable_styled_text(fallback, Vec::new(), cx);
        div()
            .min_w_0()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(fallback)
            .into_any_element()
    }

    fn data_image(&mut self, resource: &ResolvedResource) -> Option<Arc<Image>> {
        if resource.kind != ResourceKind::DataImage {
            return None;
        }
        self.image_cache_epoch = self.image_cache_epoch.saturating_add(1).max(1);
        if let Some(entry) = self.image_cache.get_mut(&resource.source) {
            entry.epoch = self.image_cache_epoch;
            return Some(entry.image.clone());
        }
        let data = resource.resolved.as_deref()?;
        let (header, encoded) = data.split_once(',')?;
        let header = header.to_ascii_lowercase();
        let (mime, encoding) = header.split_once(';')?;
        if encoding != "base64" {
            return None;
        }
        let format = ImageFormat::from_mime_type(mime.strip_prefix("data:")?)?;
        let encoded = encoded
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect::<Vec<_>>();
        let bytes = BASE64_STANDARD.decode(encoded).ok()?;
        if bytes.is_empty() || bytes.len() > DATA_IMAGE_DECODED_BYTES {
            return None;
        }
        let raster_format = match mime {
            "data:image/png" => ::image::ImageFormat::Png,
            "data:image/jpeg" => ::image::ImageFormat::Jpeg,
            "data:image/gif" => ::image::ImageFormat::Gif,
            "data:image/webp" => ::image::ImageFormat::WebP,
            _ => return None,
        };
        if !bounded_raster_dimensions(&bytes, raster_format) {
            return None;
        }
        let resident_bytes = resource.source.len().saturating_add(bytes.len());
        if resident_bytes > DATA_IMAGE_CACHE_BYTES {
            return None;
        }
        let image = Arc::new(Image::from_bytes(format, bytes));
        while self.image_cache.len() >= DATA_IMAGE_CACHE_ENTRIES
            || self.image_cache_bytes.saturating_add(resident_bytes) > DATA_IMAGE_CACHE_BYTES
        {
            let Some(oldest) = self
                .image_cache
                .iter()
                .min_by_key(|(_, entry)| entry.epoch)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(entry) = self.image_cache.remove(&oldest) {
                self.image_cache_bytes = self.image_cache_bytes.saturating_sub(entry.bytes);
            }
        }
        self.image_cache.insert(
            resource.source.clone(),
            CachedDataImage {
                image: image.clone(),
                bytes: resident_bytes,
                epoch: self.image_cache_epoch,
            },
        );
        self.image_cache_bytes = self.image_cache_bytes.saturating_add(resident_bytes);
        Some(image)
    }

    fn render_code(
        &mut self,
        node_id: NodeId,
        language: Option<&str>,
        source: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_dark = cx.theme().is_dark();
        let styles = self.highlight_styles(node_id, language, source, is_dark);
        let copy = source.to_string();
        let source_text = self.selectable_styled_text(
            SharedString::from(source.to_string()),
            styles.as_ref().clone(),
            cx,
        );
        v_flex()
            .w_full()
            .min_w_0()
            .rounded(px(6.0))
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted.opacity(if is_dark { 0.42 } else { 0.55 }))
            .child(
                h_flex()
                    .h(px(30.0))
                    .w_full()
                    .justify_between()
                    .px_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(language.unwrap_or("text").to_string()),
                    )
                    .child(
                        Button::new(format!("markdown-copy-code:{}", node_id.0))
                            .xsmall()
                            .ghost()
                            .compact()
                            .icon(IconName::Copy)
                            .tooltip("Copy code")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                            }),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .overflow_x_scrollbar()
                    .p_3()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(cx.theme().mono_font_size)
                    .line_height(px(f32::from(cx.theme().mono_font_size) + 6.0))
                    .child(source_text),
            )
            .into_any_element()
    }

    fn highlight_styles(
        &mut self,
        node_id: NodeId,
        language: Option<&str>,
        source: &str,
        is_dark: bool,
    ) -> Arc<Vec<(Range<usize>, HighlightStyle)>> {
        self.highlight_cache
            .entry((node_id, is_dark))
            .or_insert_with(|| {
                let rope = Rope::from_str(source);
                let language = normalize_code_language(language.unwrap_or("text"));
                let mut highlighter = SyntaxHighlighter::new(&language);
                highlighter.update(None, &rope, Some(CODE_HIGHLIGHT_TIMEOUT));
                let theme = if is_dark {
                    HighlightTheme::default_dark()
                } else {
                    HighlightTheme::default_light()
                };
                Arc::new(highlighter.styles(&(0..source.len()), &theme))
            })
            .clone()
    }

    fn render_diff(&mut self, node_id: NodeId, source: &str, cx: &mut Context<Self>) -> AnyElement {
        let copy = source.to_string();
        let styles = self.highlight_styles(node_id, Some("diff"), source, cx.theme().is_dark());
        let mut offset = 0_usize;
        let lines = source
            .lines()
            .map(|line| {
                let start = offset;
                offset = offset.saturating_add(line.len()).saturating_add(1);
                (start, line.to_string())
            })
            .collect::<Vec<_>>();
        let mut line_elements = Vec::with_capacity(lines.len());
        for (line_index, (line_start, line)) in lines.into_iter().enumerate() {
            if line_index > 0 {
                self.selection_inline_break();
            }
            let (background, foreground) = if line.starts_with('+') && !line.starts_with("+++") {
                (cx.theme().success.opacity(0.13), cx.theme().success)
            } else if line.starts_with('-') && !line.starts_with("---") {
                (cx.theme().danger.opacity(0.13), cx.theme().danger)
            } else if line.starts_with("@@") {
                (cx.theme().info.opacity(0.12), cx.theme().info)
            } else if line.starts_with("diff ")
                || line.starts_with("index ")
                || line.starts_with("---")
                || line.starts_with("+++")
            {
                (cx.theme().info.opacity(0.07), cx.theme().info)
            } else {
                (cx.theme().transparent, cx.theme().muted_foreground)
            };
            let line_end = line_start.saturating_add(line.len());
            let highlights = styles
                .iter()
                .filter_map(|(range, style)| {
                    let start = range.start.max(line_start);
                    let end = range.end.min(line_end);
                    (start < end).then(|| (start - line_start..end - line_start, *style))
                })
                .collect::<Vec<_>>();
            let text = self.selectable_styled_text(SharedString::from(line), highlights, cx);
            line_elements.push(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_nowrap()
                    .px_2()
                    .py_0p5()
                    .bg(background)
                    .text_color(foreground)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(cx.theme().mono_font_size)
                    .child(text),
            );
        }
        v_flex()
            .w_full()
            .min_w_0()
            .rounded(px(6.0))
            .border_1()
            .border_color(cx.theme().border)
            .overflow_hidden()
            .child(
                h_flex()
                    .h(px(30.0))
                    .justify_between()
                    .px_2()
                    .bg(cx.theme().muted)
                    .child(div().text_xs().child("diff"))
                    .child(
                        Button::new(format!("markdown-copy-diff:{}", node_id.0))
                            .xsmall()
                            .ghost()
                            .compact()
                            .icon(IconName::Copy)
                            .tooltip("Copy diff")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                            }),
                    ),
            )
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .overflow_x_scrollbar()
                    .children(line_elements),
            )
            .into_any_element()
    }

    fn render_artifact(
        &mut self,
        node_id: NodeId,
        kind: ArtifactKind,
        source: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let source_mode = matches!(kind, ArtifactKind::Mermaid | ArtifactKind::PlantUml)
            && self.diagram_source.contains(&node_id);
        let copy = source.to_string();
        let toggle_id = node_id;
        enum BodyState {
            Ready(Arc<RenderImage>, Arc<SvgArtifact>),
            Failed(String),
            Loading,
            Missing,
        }
        let state = match self.artifact_states.get(&node_id) {
            Some(ArtifactDisplayState::Ready {
                image, artifact, ..
            }) => BodyState::Ready(image.clone(), artifact.clone()),
            Some(ArtifactDisplayState::Failed { message, .. }) => {
                BodyState::Failed(message.clone())
            }
            Some(ArtifactDisplayState::Loading { .. }) => BodyState::Loading,
            None => BodyState::Missing,
        };
        let body = if source_mode {
            self.artifact_source_body(source, cx)
        } else {
            match state {
                BodyState::Ready(image, artifact) => {
                    self.push_selection_source(source);
                    if kind == ArtifactKind::InlineMath {
                        let height = artifact.height_px.clamp(18.0, 64.0);
                        let scale = height / artifact.height_px;
                        let width = (artifact.width_px * scale).clamp(1.0, 1_024.0);
                        let top = (-artifact.baseline_offset_px.unwrap_or_default() * scale)
                            .clamp(-height * 0.5, height * 0.5);
                        img(ImageSource::Render(image))
                            .w(px(width))
                            .h(px(height))
                            .max_w_full()
                            .flex_none()
                            .relative()
                            .top(px(top))
                            .object_fit(ObjectFit::Contain)
                            .into_any_element()
                    } else {
                        img(ImageSource::Render(image))
                            .max_w_full()
                            .max_h(px(640.0))
                            .object_fit(ObjectFit::Contain)
                            .into_any_element()
                    }
                }
                BodyState::Failed(message) => v_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .child(div().text_xs().text_color(cx.theme().danger).child(message))
                    .child(self.artifact_source_body(source, cx))
                    .into_any_element(),
                BodyState::Loading => h_flex()
                    .min_w_0()
                    .gap_2()
                    .text_color(cx.theme().muted_foreground)
                    .child(Spinner::new().xsmall())
                    .child(self.artifact_source_body(source, cx))
                    .into_any_element(),
                BodyState::Missing => self.artifact_source_body(source, cx),
            }
        };
        if kind == ArtifactKind::InlineMath {
            return body;
        }
        v_flex()
            .w_full()
            .min_w_0()
            .rounded(px(6.0))
            .border_1()
            .border_color(cx.theme().border)
            .overflow_hidden()
            .child(
                h_flex()
                    .h(px(32.0))
                    .w_full()
                    .justify_end()
                    .gap_1()
                    .px_2()
                    .bg(cx.theme().muted)
                    .when(
                        matches!(kind, ArtifactKind::Mermaid | ArtifactKind::PlantUml),
                        |this| {
                            this.child(
                                Button::new(format!("markdown-artifact-mode:{}", node_id.0))
                                    .xsmall()
                                    .ghost()
                                    .compact()
                                    .selected(source_mode)
                                    .icon(if source_mode {
                                        IconName::Eye
                                    } else {
                                        IconName::File
                                    })
                                    .tooltip(if source_mode {
                                        "Show rendered diagram"
                                    } else {
                                        "Show diagram source"
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.toggle_diagram_source(toggle_id, cx)
                                    })),
                            )
                        },
                    )
                    .child(
                        Button::new(format!("markdown-copy-artifact:{}", node_id.0))
                            .xsmall()
                            .ghost()
                            .compact()
                            .icon(IconName::Copy)
                            .tooltip("Copy source")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                            }),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .overflow_x_scrollbar()
                    .p_3()
                    .child(body),
            )
            .into_any_element()
    }

    fn artifact_source_body(&mut self, source: &str, cx: &mut Context<Self>) -> AnyElement {
        let source =
            self.selectable_styled_text(SharedString::from(source.to_string()), Vec::new(), cx);
        div()
            .w_full()
            .min_w_0()
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(cx.theme().mono_font_size)
            .text_color(cx.theme().muted_foreground)
            .child(source)
            .into_any_element()
    }

    fn render_progress(
        &mut self,
        node_id: NodeId,
        value: f64,
        max: f64,
        label: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ratio = if max.is_finite() && max > 0.0 {
            (value / max).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let label = label
            .map(|label| self.selectable_styled_text(SharedString::from(label), Vec::new(), cx));
        v_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .when_some(label, |this, label| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
            })
            .child(
                Progress::new(format!("markdown-progress:{}", node_id.0))
                    .xsmall()
                    .color(cx.theme().success)
                    .value(ratio * 100.0),
            )
            .into_any_element()
    }

    fn render_toc(&self, cx: &App) -> AnyElement {
        v_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .children(self.document.outline.iter().map(|entry| {
                let anchor = self.anchors.get(&entry.node_id).cloned();
                Button::new(format!("markdown-toc:{}", entry.node_id.0))
                    .ghost()
                    .small()
                    .label(entry.title.clone())
                    .ml(px(f32::from(entry.level.saturating_sub(1)) * 12.0))
                    .on_click(move |_, window, cx| {
                        if let Some(anchor) = &anchor {
                            anchor.scroll_to(window, cx);
                        }
                    })
            }))
            .text_color(cx.theme().foreground)
            .into_any_element()
    }

    fn render_table(
        &mut self,
        alignments: &[TableAlignment],
        header: Option<&crate::model::TableRow>,
        rows: &[crate::model::TableRow],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut table = v_flex()
            .w_full()
            .min_w_0()
            .overflow_x_scrollbar()
            .rounded(px(6.0))
            .border_1()
            .border_color(cx.theme().border);
        if let Some(header) = header {
            table = table.child(self.render_table_row(header, alignments, true, window, cx));
        }
        for row in rows {
            table = table.child(self.render_table_row(row, alignments, false, window, cx));
        }
        table.into_any_element()
    }

    fn render_table_row(
        &mut self,
        row: &crate::model::TableRow,
        alignments: &[TableAlignment],
        header: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.selection_block_break();
        h_flex()
            .min_w(px(320.0))
            .w_full()
            .items_stretch()
            .border_b_1()
            .border_color(cx.theme().border)
            .children(row.cells.iter().enumerate().map(|(index, cell)| {
                if index > 0 {
                    self.selection_text.push('\t');
                }
                let alignment = alignments
                    .get(index)
                    .copied()
                    .unwrap_or(TableAlignment::None);
                div()
                    .flex_1()
                    .min_w(px(120.0))
                    .p_2()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .when(header, |this| {
                        this.bg(cx.theme().muted).font_weight(FontWeight::SEMIBOLD)
                    })
                    .when(alignment == TableAlignment::Center, |this| {
                        this.text_center()
                    })
                    .when(alignment == TableAlignment::Right, |this| this.text_right())
                    .child(self.render_inlines(cell, window, cx))
            }))
            .into_any_element()
    }
}

fn bounded_raster_dimensions(bytes: &[u8], format: ::image::ImageFormat) -> bool {
    let reader = ::image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    let Ok((width, height)) = reader.into_dimensions() else {
        return false;
    };
    width > 0
        && height > 0
        && width <= DATA_IMAGE_MAX_DIMENSION
        && height <= DATA_IMAGE_MAX_DIMENSION
        && u64::from(width)
            .saturating_mul(u64::from(height))
            .saturating_mul(4)
            <= DATA_IMAGE_MAX_RGBA_BYTES
}

fn artifact_state_key(state: &ArtifactDisplayState) -> ArtifactKey {
    match state {
        ArtifactDisplayState::Loading { key }
        | ArtifactDisplayState::Ready { key, .. }
        | ArtifactDisplayState::Failed { key, .. } => *key,
    }
}

fn collect_document_state(
    blocks: &[BlockNode],
    live: &mut BTreeSet<NodeId>,
    artifacts: &mut Vec<ArtifactSpec>,
    artifact_limit: usize,
) {
    for block in blocks {
        live.insert(block.id);
        match &block.kind {
            Block::Paragraph(inlines)
            | Block::Heading {
                content: inlines, ..
            } => collect_inline_state(inlines, live, artifacts, artifact_limit),
            Block::Quote(children)
            | Block::Callout { children, .. }
            | Block::Details { children, .. }
            | Block::FootnoteDefinition { children, .. }
            | Block::SafeHtml(children) => {
                collect_document_state(children, live, artifacts, artifact_limit)
            }
            Block::Math { source } if artifacts.len() < artifact_limit => {
                artifacts.push(ArtifactSpec {
                    node_id: block.id,
                    kind: ArtifactKind::DisplayMath,
                    source: Arc::from(source.as_str()),
                });
            }
            Block::Diagram { kind, source } if artifacts.len() < artifact_limit => {
                artifacts.push(ArtifactSpec {
                    node_id: block.id,
                    kind: match kind {
                        DiagramKind::Mermaid => ArtifactKind::Mermaid,
                        DiagramKind::PlantUml => ArtifactKind::PlantUml,
                    },
                    source: Arc::from(source.as_str()),
                });
            }
            Block::List { items, .. } => {
                for item in items {
                    live.insert(item.id);
                    collect_document_state(&item.children, live, artifacts, artifact_limit);
                }
            }
            Block::DefinitionList(items) => {
                for item in items {
                    collect_inline_state(&item.term, live, artifacts, artifact_limit);
                    for definition in &item.definitions {
                        collect_document_state(definition, live, artifacts, artifact_limit);
                    }
                }
            }
            Block::Table { header, rows, .. } => {
                for row in header.iter().chain(rows) {
                    for cell in &row.cells {
                        collect_inline_state(cell, live, artifacts, artifact_limit);
                    }
                }
            }
            _ => {}
        }
    }
}

fn collect_inline_state(
    inlines: &[InlineNode],
    live: &mut BTreeSet<NodeId>,
    artifacts: &mut Vec<ArtifactSpec>,
    artifact_limit: usize,
) {
    for inline in inlines {
        live.insert(inline.id);
        if let Inline::Math(source) = &inline.kind
            && artifacts.len() < artifact_limit
        {
            artifacts.push(ArtifactSpec {
                node_id: inline.id,
                kind: ArtifactKind::InlineMath,
                source: Arc::from(source.as_str()),
            });
        }
        if let Some(children) = inline.kind.children() {
            collect_inline_state(children, live, artifacts, artifact_limit);
        }
    }
}

fn initialize_details(blocks: &[BlockNode], open: &mut BTreeMap<NodeId, bool>) {
    for block in blocks {
        if let Block::Details {
            initially_open,
            children,
            ..
        } = &block.kind
        {
            open.entry(block.id).or_insert(*initially_open);
            initialize_details(children, open);
        } else if let Some(children) = block.kind.children() {
            initialize_details(children, open);
        }
    }
}

fn normalize_code_language(language: &str) -> String {
    let language = language
        .trim()
        .split_ascii_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match language.as_str() {
        "shell" | "shellscript" | "zsh" => "bash".into(),
        "patch" => "diff".into(),
        "cxx" | "cc" | "hpp" | "hxx" => "cpp".into(),
        "h" => "c".into(),
        "c#" | "dotnet" => "csharp".into(),
        "node" | "nodejs" | "jsx" => "javascript".into(),
        "htm" | "xhtml" | "xml" => "html".into(),
        "mdown" | "mkd" => "markdown".into(),
        "postgres" | "postgresql" | "sqlite" => "sql".into(),
        "rb" => "ruby".into(),
        "rs" => "rust".into(),
        "py" => "python".into(),
        "yml" => "yaml".into(),
        _ if language.is_empty() => "text".into(),
        _ => language,
    }
}

fn search_highlights(
    text: &str,
    query: Option<&str>,
    cx: &App,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let Some(query) = query.filter(|query| !query.is_empty()) else {
        return Vec::new();
    };
    let ranges = case_insensitive_match_ranges(text, query, 1_024);
    if ranges.is_empty() {
        return Vec::new();
    }
    ranges
        .into_iter()
        .map(|range| {
            (
                range,
                HighlightStyle {
                    background_color: Some(cx.theme().warning.opacity(0.38)),
                    font_weight: Some(FontWeight::BOLD),
                    ..Default::default()
                },
            )
        })
        .collect()
}

fn case_insensitive_match_ranges(text: &str, query: &str, limit: usize) -> Vec<Range<usize>> {
    if text.is_empty() || query.is_empty() || limit == 0 {
        return Vec::new();
    }
    let folded_query = query.to_lowercase();
    if folded_query.is_empty() {
        return Vec::new();
    }
    let mut folded_text = String::new();
    let mut character_ranges = Vec::new();
    for (original_start, character) in text.char_indices() {
        let folded_start = folded_text.len();
        folded_text.extend(character.to_lowercase());
        character_ranges.push((
            folded_start..folded_text.len(),
            original_start..original_start + character.len_utf8(),
        ));
    }
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for (folded_start, _) in folded_text.match_indices(&folded_query).take(limit) {
        let folded_end = folded_start + folded_query.len();
        let first = character_ranges.partition_point(|(folded, _)| folded.end <= folded_start);
        let last = character_ranges.partition_point(|(folded, _)| folded.start < folded_end);
        if first >= character_ranges.len() || last == 0 {
            continue;
        }
        let next = character_ranges[first].1.start..character_ranges[last - 1].1.end;
        if let Some(previous) = ranges.last_mut()
            && previous.end >= next.start
        {
            previous.end = previous.end.max(next.end);
        } else {
            ranges.push(next);
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use ::gpui::{
        InteractiveElement as _, Modifiers, MouseButton, StatefulInteractiveElement as _,
        TestAppContext, VisualTestContext,
    };
    use gpui_component::{ElementExt as _, Theme, ThemeMode};

    use super::*;
    use crate::MarkdownSurface;

    struct MarkdownLayoutProbe {
        input: MarkdownInput,
        width: f32,
        measured_width: Rc<Cell<f32>>,
        height: Rc<Cell<f32>>,
    }

    struct MarkdownSelectionProbe {
        input: MarkdownInput,
        state: Entity<MarkdownViewState>,
        width: f32,
    }

    struct MarkdownVirtualizationProbe {
        input: MarkdownInput,
        state: Entity<MarkdownViewState>,
        scroll: ScrollHandle,
    }

    impl MarkdownSelectionProbe {
        fn new(input: MarkdownInput, cx: &mut Context<Self>) -> Self {
            let state = cx.new(|cx| {
                MarkdownViewState::new(
                    "selection-probe".into(),
                    input.clone(),
                    None,
                    MarkdownViewOptions::default(),
                    cx,
                )
            });
            Self {
                input,
                state,
                width: 560.0,
            }
        }

        fn with_width(mut self, width: f32) -> Self {
            self.width = width;
            self
        }
    }

    impl MarkdownVirtualizationProbe {
        fn new(input: MarkdownInput, cx: &mut Context<Self>) -> Self {
            let scroll = ScrollHandle::new();
            let document = Arc::new(parse_markdown(input.clone()));
            let state = cx.new(|cx| {
                MarkdownViewState::new(
                    "virtualization-probe".into(),
                    input.clone(),
                    Some(document),
                    MarkdownViewOptions {
                        presentation: MarkdownPresentation::Agent,
                        scroll_handle: Some(scroll.clone()),
                        ..MarkdownViewOptions::default()
                    },
                    cx,
                )
            });
            Self {
                input,
                state,
                scroll,
            }
        }
    }

    impl Render for MarkdownSelectionProbe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let mut markdown = MarkdownView::new("selection-probe", self.input.clone());
            markdown.state = Some(self.state.clone());
            div().w(px(self.width)).child(markdown)
        }
    }

    impl Render for MarkdownVirtualizationProbe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let mut markdown = MarkdownView::new("virtualization-probe", self.input.clone())
                .presentation(MarkdownPresentation::Agent)
                .scroll_handle(self.scroll.clone());
            markdown.state = Some(self.state.clone());
            div()
                .id("virtualization-probe-scroll")
                .w(px(560.0))
                .h(px(240.0))
                .track_scroll(&self.scroll)
                .overflow_y_scroll()
                .child(markdown)
        }
    }

    impl Render for MarkdownLayoutProbe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let height = self.height.clone();
            let measured_width = self.measured_width.clone();
            div().w(px(self.width)).child(
                div()
                    .w_full()
                    .on_prepaint(move |bounds, _, _| {
                        measured_width.set(f32::from(bounds.size.width));
                        height.set(f32::from(bounds.size.height));
                    })
                    .child(MarkdownView::new(
                        "markdown-layout-probe",
                        self.input.clone(),
                    )),
            )
        }
    }

    #[test]
    fn shared_markdown_text_comparison_keeps_value_semantics() {
        let source: Arc<str> = Arc::from("a long unchanged markdown source");
        let same_allocation = source.clone();
        let equal_value: Arc<str> = Arc::from("a long unchanged markdown source");
        let changed: Arc<str> = Arc::from("changed markdown source");

        assert!(Arc::ptr_eq(&source, &same_allocation));
        assert!(markdown_text_matches(&source, &same_allocation));
        assert!(!Arc::ptr_eq(&source, &equal_value));
        assert!(markdown_text_matches(&source, &equal_value));
        assert!(!markdown_text_matches(&source, &changed));
    }

    #[test]
    fn virtual_markdown_block_lookup_and_measurement_stay_indexed() {
        let source = include_str!("gpui_view.rs");
        let lookup = source
            .split_once("    fn visible_virtual_blocks(")
            .and_then(|(_, tail)| tail.split_once("\n    fn prepare_virtual_selection("))
            .map(|(body, _)| body)
            .expect("virtual block lookup should remain inspectable");
        assert!(lookup.contains("partition_point"));
        assert!(!lookup.contains(".position("));

        let measurement = source
            .split_once("    fn record_virtual_block_heights(")
            .and_then(|(_, tail)| tail.split_once("\n    fn rebuild_anchors("))
            .map(|(body, _)| body)
            .expect("virtual block measurement should remain inspectable");
        assert_eq!(measurement.matches("Arc::make_mut").count(), 2);
        assert!(!measurement.contains("virtual_block_sizes.as_ref().clone()"));
    }

    #[test]
    fn inline_semantic_highlights_are_explicit_and_composable() {
        let semantic = InlineSemanticStyle::default()
            .strong()
            .emphasis()
            .deletion()
            .underline();
        let highlights = combine_inline_highlights(
            6,
            semantic,
            vec![(
                2..4,
                HighlightStyle {
                    font_weight: Some(FontWeight::BOLD),
                    ..Default::default()
                },
            )],
        );

        assert!(!highlights.is_empty());
        assert!(highlights.iter().all(|(_, style)| {
            style.font_weight == Some(FontWeight::BOLD)
                && style.font_style == Some(FontStyle::Italic)
                && style.underline.is_some()
                && style.strikethrough.is_some()
        }));
    }

    #[test]
    fn selection_background_overrides_semantic_background_without_losing_text_style() {
        let semantic_background = ::gpui::hsla(0.0, 0.0, 0.25, 1.0);
        let selection_background = ::gpui::hsla(0.58, 0.8, 0.5, 0.65);
        let highlights = overlay_selection_highlight(
            12,
            vec![(
                4..10,
                HighlightStyle {
                    background_color: Some(semantic_background),
                    font_weight: Some(FontWeight::BOLD),
                    ..Default::default()
                },
            )],
            2..7,
            selection_background,
        );

        assert_eq!(highlights.len(), 3);
        assert_eq!(highlights[0].0, 2..4);
        assert_eq!(highlights[0].1.background_color, Some(selection_background));
        assert_eq!(highlights[1].0, 4..7);
        assert_eq!(highlights[1].1.background_color, Some(selection_background));
        assert_eq!(highlights[1].1.font_weight, Some(FontWeight::BOLD));
        assert_eq!(highlights[2].0, 7..10);
        assert_eq!(highlights[2].1.background_color, Some(semantic_background));
    }

    #[test]
    fn plain_text_view_keeps_markdown_syntax_in_one_selectable_text_flow() {
        let source = "# heading **bold** `code` [link](https://example.com)";
        let view =
            MarkdownView::plain_text("plain-text-contract", MarkdownInput::new(source, "", 1));
        let document = view.document.expect("plain text document");
        let [block] = document.blocks.as_ref() else {
            panic!("plain text should render as one block");
        };
        let Block::Paragraph(inlines) = &block.kind else {
            panic!("plain text should render as one paragraph");
        };
        let [inline] = inlines.as_slice() else {
            panic!("plain text should render as one inline segment");
        };
        assert_eq!(inline.kind, Inline::Text(source.to_string()));
        assert_eq!(document.plain_text(), source);
        assert!(document.resources.is_empty());
    }

    #[::gpui::test]
    fn long_agent_markdown_only_materializes_viewport_blocks(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let source = (0..120)
            .map(|index| {
                format!(
                    "## Section {index}\n\nParagraph {index} contains enough text to wrap in the Agent message viewport and exercise block measurement."
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let input = MarkdownInput::new(source, "", 1).surface(MarkdownSurface::Agent);
        let (probe, cx) = cx.add_window_view(|_, cx| MarkdownVirtualizationProbe::new(input, cx));

        for _ in 0..4 {
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            cx.run_until_parked();
        }

        let state = probe.read_with(cx, |probe, _| probe.state.clone());
        let (block_count, first_range, total_height) = state.read_with(cx, |state, _| {
            (
                state.document.blocks.len(),
                state.virtual_visible_blocks.clone().unwrap_or_default(),
                state.virtual_total_height(),
            )
        });
        assert!(block_count >= 200);
        assert!(!first_range.is_empty());
        assert!(first_range.len() < block_count / 4);
        assert!(total_height > px(240.0));

        let scroll = probe.read_with(cx, |probe, _| probe.scroll.clone());
        scroll.set_offset(point(px(0.0), px(-4_000.0)));
        for _ in 0..4 {
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
            cx.run_until_parked();
        }

        let second_range = state.read_with(cx, |state, _| {
            state.virtual_visible_blocks.clone().unwrap_or_default()
        });
        assert!(second_range.start > first_range.start);
        assert!(second_range.len() < block_count / 4);
    }

    #[::gpui::test]
    fn heading_levels_keep_their_own_typography(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let input = MarkdownInput::new(
            "# HeadingOne\n\n## HeadingTwo\n\n### HeadingThree\n\n#### HeadingFour\n\n##### HeadingFive\n\n###### HeadingSix\n\nBodyText",
            "",
            1,
        );
        let (probe, cx) = cx.add_window_view(|_, cx| MarkdownSelectionProbe::new(input, cx));
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let state = probe.read_with(cx, |probe, _| probe.state.clone());
        state.read_with(cx, |state, _| {
            let line_height = |needle: &str| {
                state
                    .selection_segments
                    .values()
                    .find(|segment| {
                        state.selection_text.get(segment.text_range.clone()) == Some(needle)
                    })
                    .map(|segment| f32::from(segment.layout.line_height()))
                    .expect("heading selection segment")
            };
            let heading_heights = [
                line_height("HeadingOne"),
                line_height("HeadingTwo"),
                line_height("HeadingThree"),
                line_height("HeadingFour"),
                line_height("HeadingFive"),
                line_height("HeadingSix"),
            ];
            assert!(
                heading_heights.windows(2).all(|pair| pair[0] > pair[1]),
                "heading line heights must descend by level: {heading_heights:?}"
            );
            assert_eq!(heading_heights[5], line_height("BodyText"));
        });
    }

    #[::gpui::test]
    fn native_view_renders_a_nonblank_long_document(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let height = Rc::new(Cell::new(0.0));
        let observed = height.clone();
        let measured_width = Rc::new(Cell::new(0.0));
        let source = (0..20)
            .map(|index| format!("## Section {index}\n\nText with **strong** and `code`."))
            .collect::<Vec<_>>()
            .join("\n\n");
        let (_, cx) = cx.add_window_view(|_, _| MarkdownLayoutProbe {
            input: MarkdownInput::new(source, "", 1),
            width: 640.0,
            measured_width,
            height,
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(observed.get() > 288.0);
    }

    #[::gpui::test]
    fn inline_styles_share_one_wrapping_text_layout(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let input = MarkdownInput::new(
            "Before **bold** and `code` plus [link](https://example.com) after.",
            "",
            1,
        );
        let (probe, cx) = cx.add_window_view(|_, cx| MarkdownSelectionProbe::new(input, cx));
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let state = probe.read_with(cx, |probe, _| probe.state.clone());
        state.read_with(cx, |state, _| {
            let expected = "Before bold and code plus link after.";
            let segments = state
                .selection_segments
                .values()
                .filter(|segment| {
                    state.selection_text.get(segment.text_range.clone()) == Some(expected)
                })
                .collect::<Vec<_>>();
            assert_eq!(segments.len(), 1);
            assert_eq!(segments[0].layout.wrapped_text(), expected);
        });
    }

    #[::gpui::test]
    fn chinese_list_items_keep_inline_code_and_links_in_the_same_line_layout(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let source = concat!(
            "- 用户手动点“停止”，目前会话会被置为 `Idle`，见 ",
            "[manager.rs](/workspace/crates/agent/src/manager.rs:1795)。\n",
            "- ACP 返回 `cancelled`、`interrupted` 等停止原因。\n",
            "- 普通执行结束、等待权限输入，或仅出现前端 `agent_error` 临时提示。",
        );
        let input = MarkdownInput::new(source, "/workspace", 1).surface(MarkdownSurface::Agent);
        let document = parse_markdown(input.clone());
        let list_items_support_text_flow = document.blocks.iter().all(|block| {
            let Block::List { items, .. } = &block.kind else {
                return false;
            };
            items.iter().all(|item| {
                item.children.iter().all(|child| {
                    matches!(
                        &child.kind,
                        Block::Paragraph(inlines)
                            if inlines.iter().all(inline_supports_text_flow)
                    )
                })
            })
        });
        assert!(
            list_items_support_text_flow,
            "fixture should use the combined inline text flow: {:#?}",
            document.blocks,
        );
        let (probe, cx) =
            cx.add_window_view(|_, cx| MarkdownSelectionProbe::new(input, cx).with_width(760.0));
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let state = probe.read_with(cx, |probe, _| probe.state.clone());
        state.read_with(cx, |state, _| {
            for expected in [
                "用户手动点“停止”，目前会话会被置为 Idle，见 manager.rs。",
                "ACP 返回 cancelled、interrupted 等停止原因。",
                "普通执行结束、等待权限输入，或仅出现前端 agent_error 临时提示。",
            ] {
                let segment = state
                    .selection_segments
                    .values()
                    .find(|segment| {
                        state.selection_text.get(segment.text_range.clone()) == Some(expected)
                    })
                    .unwrap_or_else(|| {
                        let segments = state
                            .selection_segments
                            .values()
                            .filter_map(|segment| {
                                state
                                    .selection_text
                                    .get(segment.text_range.clone())
                                    .map(|text| (text, segment.layout.wrapped_text()))
                            })
                            .collect::<Vec<_>>();
                        panic!(
                            "list item should use one selectable text segment; expected={expected:?}, selection={:?}, segments={segments:?}",
                            state.selection_text,
                        );
                    });
                assert_eq!(segment.layout.wrapped_text(), expected);
            }
        });
    }

    #[::gpui::test]
    fn native_text_drag_selects_across_styles_and_paragraphs_and_copies(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let input = MarkdownInput::new(
            "# Heading\n\nFirst **bold** text.\n\nSecond paragraph.",
            "",
            1,
        );
        let (probe, cx) = cx.add_window_view(|_, cx| MarkdownSelectionProbe::new(input, cx));
        let cx: &mut VisualTestContext = cx;
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let state = probe.read_with(cx, |probe, _| probe.state.clone());
        let (start, end) = state.read_with(cx, |state, _| {
            let segment_for = |needle: &str| {
                state
                    .selection_segments
                    .values()
                    .find(|segment| {
                        state.selection_text.get(segment.text_range.clone()) == Some(needle)
                    })
                    .expect("fixture selection segment")
            };
            let first = segment_for("First bold text.");
            let second = segment_for("Second paragraph.");
            let start = first.layout.position_for_index(0).unwrap()
                + point(px(1.0), first.layout.line_height() / 2.0);
            let end = second.layout.position_for_index("Second".len()).unwrap()
                + point(px(0.0), second.layout.line_height() / 2.0);
            (start, end)
        });

        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::default());
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        assert_eq!(
            state.read_with(cx, |state, _| state.selected_text()),
            "First bold text.\nSecond"
        );

        cx.dispatch_action(CopyAction);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("First bold text.\nSecond".to_string())
        );

        cx.dispatch_action(SelectAll);
        state.read_with(cx, |state, _| {
            assert_eq!(state.selected_text(), state.selection_text);
            assert!(state.selected_text().contains("Heading\nFirst bold text."));
        });
    }

    #[::gpui::test]
    fn native_view_renders_the_dark_fixture_at_narrow_width(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| Theme::change(ThemeMode::Dark, None, cx));
        let measured_width = Rc::new(Cell::new(0.0));
        let observed_width = measured_width.clone();
        let height = Rc::new(Cell::new(0.0));
        let observed_height = height.clone();
        let (_, cx) = cx.add_window_view(|_, _| MarkdownLayoutProbe {
            input: MarkdownInput::new(include_str!("../fixtures/advanced.md"), "docs", 2),
            width: 320.0,
            measured_width,
            height,
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert!(observed_width.get() > 0.0 && observed_width.get() <= 320.0);
        assert!(observed_height.get() > 640.0);
    }

    #[::gpui::test]
    fn artifact_state_tracks_the_latest_theme_during_an_existing_job(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| Theme::change(ThemeMode::Light, None, cx));
        let (state, cx) = cx.add_window_view(|_, cx| {
            MarkdownViewState::new(
                "artifact-theme-race".into(),
                MarkdownInput::new("Inline $E = mc^2$.", "", 1),
                None,
                MarkdownViewOptions::default(),
                cx,
            )
        });

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let light_key = state.read_with(cx, |state, _| {
            artifact_state_key(state.artifact_states.values().next().unwrap())
        });

        cx.update(|_, cx| Theme::change(ThemeMode::Dark, None, cx));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let dark_key = state.read_with(cx, |state, _| {
            artifact_state_key(state.artifact_states.values().next().unwrap())
        });
        assert_ne!(light_key, dark_key);

        cx.update(|_, cx| Theme::change(ThemeMode::Light, None, cx));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let restored_light_key = state.read_with(cx, |state, _| {
            artifact_state_key(state.artifact_states.values().next().unwrap())
        });
        assert_eq!(restored_light_key, light_key);
    }

    #[::gpui::test]
    async fn native_view_rasterizes_every_fixture_artifact(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| Theme::change(ThemeMode::Dark, None, cx));
        let (state, cx) = cx.add_window_view(|_, cx| {
            MarkdownViewState::new(
                "artifact-fixture".into(),
                MarkdownInput::new(include_str!("../fixtures/advanced.md"), "docs", 3),
                None,
                MarkdownViewOptions::default(),
                cx,
            )
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        for _ in 0..80 {
            cx.run_until_parked();
            let complete = state.read_with(cx, |state, _| {
                state.artifact_states.len() == state.artifact_specs.len()
                    && state.artifact_states.values().all(|state| {
                        matches!(
                            state,
                            ArtifactDisplayState::Ready { .. }
                                | ArtifactDisplayState::Failed { .. }
                        )
                    })
            });
            if complete {
                break;
            }
            cx.executor().timer(Duration::from_millis(100)).await;
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }

        let (expected, ready, nonblank, readable_math, failures) =
            state.read_with(cx, |state, _| {
                let ready = state
                    .artifact_states
                    .values()
                    .filter(|state| matches!(state, ArtifactDisplayState::Ready { .. }))
                    .count();
                let nonblank = state
                    .artifact_states
                    .values()
                    .filter(|state| {
                        if let ArtifactDisplayState::Ready { image, .. } = state {
                            image.as_bytes(0).is_some_and(|pixels| {
                                !pixels.is_empty() && pixels.iter().any(|byte| *byte != 0)
                            })
                        } else {
                            false
                        }
                    })
                    .count();
                let math_specs = state
                    .artifact_specs
                    .iter()
                    .filter(|spec| {
                        matches!(
                            spec.kind,
                            ArtifactKind::InlineMath | ArtifactKind::DisplayMath
                        )
                    })
                    .collect::<Vec<_>>();
                let readable_math = !math_specs.is_empty()
                    && math_specs.iter().all(|spec| {
                        let Some(ArtifactDisplayState::Ready { image, .. }) =
                            state.artifact_states.get(&spec.node_id)
                        else {
                            return false;
                        };
                        image.as_bytes(0).is_some_and(|pixels| {
                            pixels.chunks_exact(4).any(|pixel| {
                                pixel[3] > 0 && pixel[..3].iter().any(|channel| *channel >= 0xc0)
                            })
                        })
                    });
                let failures = state
                    .artifact_states
                    .values()
                    .filter_map(|state| {
                        if let ArtifactDisplayState::Failed { message, .. } = state {
                            Some(message.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                (
                    state.artifact_specs.len(),
                    ready,
                    nonblank,
                    readable_math,
                    failures,
                )
            });
        assert_eq!(expected, 4);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(ready, expected);
        assert_eq!(nonblank, expected);
        assert!(
            readable_math,
            "dark math artifacts must use a light foreground"
        );
    }

    #[::gpui::test]
    fn large_documents_start_with_a_fallback_and_accept_only_the_latest_parse(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let initial_source = format!("# Initial\n\n{}", "body\n\n".repeat(12_000));
        let observed_pending = Rc::new(Cell::new(false));
        let pending = observed_pending.clone();
        let (state, cx) = cx.add_window_view(|_, cx| {
            let state = MarkdownViewState::new(
                "large-document".into(),
                MarkdownInput::new(initial_source, "docs", 1),
                None,
                MarkdownViewOptions::default(),
                cx,
            );
            pending.set(
                state
                    .document
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == "markdown_parse_pending"),
            );
            state
        });
        assert!(observed_pending.get());

        let stale = format!("# Stale\n\n{}", "old\n\n".repeat(12_000));
        let latest = format!("# Latest\n\n{}", "new\n\n".repeat(12_000));
        state.update(cx, |state, cx| {
            state.update(
                MarkdownInput::new(stale, "docs", 2),
                None,
                MarkdownViewOptions::default(),
                cx,
            );
            state.update(
                MarkdownInput::new(latest.clone(), "docs", 3),
                None,
                MarkdownViewOptions::default(),
                cx,
            );
        });
        cx.run_until_parked();

        state.read_with(cx, |state, _| {
            assert_eq!(state.document.revision, 3);
            assert!(state.document.source.starts_with("# Latest"));
            assert!(
                state
                    .document
                    .diagnostics
                    .iter()
                    .all(|diagnostic| diagnostic.code != "markdown_parse_pending")
            );
        });
    }

    #[::gpui::test]
    fn streaming_markdown_uses_one_background_pipeline_and_catches_up_to_latest(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let options = MarkdownViewOptions {
            streaming: true,
            presentation: MarkdownPresentation::Agent,
            ..MarkdownViewOptions::default()
        };
        let observed_pending = Rc::new(Cell::new(false));
        let pending = observed_pending.clone();
        let (state, cx) = cx.add_window_view(|_, cx| {
            let state = MarkdownViewState::new(
                "streaming-document".into(),
                MarkdownInput::new("first", "", 1),
                None,
                options.clone(),
                cx,
            );
            pending.set(
                state
                    .document
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == "markdown_parse_pending"),
            );
            state
        });
        assert!(observed_pending.get());

        state.update(cx, |state, cx| {
            state.update(
                MarkdownInput::new("second", "", 2),
                None,
                options.clone(),
                cx,
            );
            state.update(MarkdownInput::new("latest", "", 3), None, options, cx);
            assert!(state.parse_task.is_some());
        });
        cx.run_until_parked();

        state.read_with(cx, |state, _| {
            assert_eq!(state.document.revision, 3);
            assert_eq!(state.document.source.as_ref(), "latest");
            assert!(state.parse_task.is_none());
        });
    }

    #[test]
    fn detail_state_initialization_preserves_existing_user_choice() {
        let document = parse_markdown(MarkdownInput::new(
            "<details open><summary>A</summary>B</details>",
            "",
            1,
        ));
        let mut open = BTreeMap::new();
        initialize_details(&document.blocks, &mut open);
        let node_id = document
            .blocks
            .iter()
            .find_map(|block| matches!(block.kind, Block::Details { .. }).then_some(block.id))
            .unwrap();
        assert_eq!(open.get(&node_id), Some(&true));
        open.insert(node_id, false);
        initialize_details(&document.blocks, &mut open);
        assert_eq!(open.get(&node_id), Some(&false));
    }

    #[test]
    fn search_ranges_are_case_insensitive_unicode_safe_and_bounded() {
        let text = "Alpha alpha İSTANBUL";
        let matches = case_insensitive_match_ranges(text, "ALPHA", 8)
            .into_iter()
            .map(|range| &text[range])
            .collect::<Vec<_>>();
        assert_eq!(matches, ["Alpha", "alpha"]);
        let unicode = case_insensitive_match_ranges(text, "i", 1);
        assert_eq!(&text[unicode[0].clone()], "İ");
        assert_eq!(case_insensitive_match_ranges("a a a", "a", 2).len(), 2);
    }

    #[test]
    fn code_language_aliases_normalize_before_highlighting() {
        assert_eq!(normalize_code_language("  C++ "), "c++");
        assert_eq!(normalize_code_language("zsh"), "bash");
        assert_eq!(normalize_code_language("patch"), "diff");
        assert_eq!(normalize_code_language("C#"), "csharp");
        assert_eq!(normalize_code_language("jsx title=demo"), "javascript");
        assert_eq!(normalize_code_language("unknown"), "unknown");
    }

    #[test]
    fn data_image_dimensions_are_validated_before_gpui_decode() {
        let png = BASE64_STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .expect("1x1 PNG fixture");

        assert!(bounded_raster_dimensions(&png, ::image::ImageFormat::Png));
        assert!(!bounded_raster_dimensions(
            b"not an image",
            ::image::ImageFormat::Png
        ));
    }
}
