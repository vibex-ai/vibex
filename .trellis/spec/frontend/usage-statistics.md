# GPUI Usage Statistics

Usage Statistics is an independent GPUI Desktop workbench surface backed by the
typed Agent usage query. It is not a Provider configuration section and does not
render raw database or ACP records.

## Scenario: Independent Usage Route And Operational View

### 1. Scope / Trigger

- Read this contract when changing the desktop sidebar, workbench routing,
  current-session usage control, Usage controller state, filters, trend, summary,
  table, accessibility, localization, or responsive behavior.
- GPUI Desktop is the current product baseline. Do not recreate this feature in
  the deleted React/Tauri desktop or bypass `BackendFacade`.

### 2. Signatures

```text
AgentBackend::usage_statistics(AgentUsageStatisticsRequest)
  -> BackendFuture<AgentUsageStatistics>

WorkbenchRoute {
  ...,
  primary_tab: "agent" | "management" | "usage",
  usage_session_id: String?
}

UsageView::set_backend(BackendFacade, cx)
UsageView::clear_backend(cx)
UsageView::activate(Option<VibexSessionId>, cx)
UsageView::invalidate(visible, cx)
```

Stable entry ids are `sidebar-usage-statistics` and `open-session-usage`. The
session entry is a standard `gpui_component::button::Button` wrapped with
`button_with_aria_label`.

### 3. Contracts

- `primary_tab = "usage"` is a first-class route. Capture/apply, back/forward,
  and persisted route decoding must preserve `usage_session_id`; legacy routes
  without the field decode it as `None`.
- The sidebar order is Config Center, Usage Statistics, separator, Projects.
  The Usage entry uses the standard Button selected state and opens an
  unfiltered Usage route.
- Usage keeps the left sidebar visible but suppresses Agent-only preview and
  right-rail surfaces, matching Management shell behavior without becoming a
  Management subsection.
- Activating the live current-session context/cache ring opens Usage with that
  `VibexSessionId` as its session filter. The ring remains live-snapshot-driven;
  navigation and durable queries must not block its updates.
- Icon-only session controls must use the standard GPUI Button plus a localized
  ARIA label and tooltip. Do not replace them with a clickable `div`.
- `UsageView` owns the typed request, last successful statistics, loading/error
  state, stale marker, monotonically increasing generation, and refresh task.
  Ignore completions from older generations. A refresh error may mark retained
  data stale but must not erase the last successful result.
- A committed usage fact invalidates the view. Refresh immediately only while
  Usage is visible; otherwise mark it stale and refresh on activation.
- The toolbar exposes Today/7d/30d/All and Agent/Model Provider/Model/Project/
  Session cross-filters in that order. The chart header exposes a local Trend/
  Heatmap/Models segmented mode control. The dimension control exposes Time/
  Agent/Project/Model Provider/Model.
- Render six stable summary metrics: total, requests, input, output, cached read,
  and cache hit rate. Each summary title has a distinct semantic icon; summary
  cards are separate bordered surfaces with the semantic card background and
  show only the title and value, with no inline helper, coverage row, or hover
  tooltip. Keep exact values and coverage in table cell tooltips and table
  coverage cells; do not render a page-level partial-reporting banner. `None`
  displays as Unknown/Not reported, never `0`.
- Trend mode is a green stacked column chart with a left K-unit Token axis. Its
  clickable legend controls Total, Input, Output, and Cache locally; Requests is
  not a trend series. Total is mutually exclusive with the detail stack because
  it already contains Input and Output. Input/Output/Cache may be enabled in any
  combination. Unknown values remain unknown in the exact-value tooltip.
- Heatmap mode always renders the latest 365 local calendar days and does not use
  the selected Today/7d/30d/All range. It still applies all five cross-filters,
  aligns Monday through Sunday, labels months, distinguishes unknown from zero,
  and uses the semantic green chart scale from low to high daily total Token.
- Models mode uses the same fixed 365-day projection and does not use the selected
  range. Each vertical column is one day and is normalized to 100%; its colored
  segments represent model shares. The user can switch the basis between Requests
  and Total Token. Rank models over the full annual projection by Total Token with
  Requests as the fallback/tie-break, keep the top nine, and merge the remainder
  into Other. The category order and colors remain stable when the basis changes.
- The table has stable column widths, horizontal scrolling, sortable metrics,
  last activity, and coverage. While horizontal overflow exists, wheel input over
  the table is converted to horizontal movement and does not bubble into the page;
  the table is not a nested vertical scroll surface. Summary columns adapt for
  narrow windows and toolbar controls wrap without overlap.
- Loading, empty, unsupported/error, ready, and stale states are explicit.
  When retained statistics refresh in the background, keep the content geometry
  stable and expose progress through the fixed-size header refresh control rather
  than inserting a transient in-flow status row.
  Partial coverage remains available in affected table rows and their tooltips.
  All visible strings and accessibility labels use the existing English/Simplified
  Chinese/Traditional Chinese locale helper. All colors use semantic theme tokens.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Backend is not installed or is cleared | Clear statistics/tasks and render an empty or unavailable state without issuing a query. |
| Backend returns `agent_usage_statistics_unavailable` | Render the explicit unsupported state. |
| Other refresh error with prior data | Keep prior statistics, mark them stale, and show bounded error status. |
| Other refresh error without prior data | Render the unavailable/error state. |
| Older async generation completes after a newer request | Ignore it with no state replacement. |
| Selected range contains zero requests | Keep summaries, charts, filters, and table reachable so the fixed annual views remain usable; render zero/unknown honestly. |
| A metric value or cache hit rate is `None` | Render Unknown/Not reported (`-` where compact), never numeric zero. |
| A Token trend segment is `None` in a bucket with requests | Omit that segment and retain Unknown in the exact-value tooltip; never fabricate zero. |
| User changes Today/7d/30d/All while Heatmap or Models is active | Keep the chart mode and its fixed 365-day data; refresh selected-range summaries/trend/table normally. |
| More than ten models occur in the annual projection | Render the top nine for the selected basis plus one Other category. |
| `usage_session_id` is absent in legacy route JSON | Decode as `None`. |
| `usage_session_id` cannot parse as `VibexSessionId` | Drop only the filter; keep the Usage route usable. |
| Filter option id cannot parse into its typed id | Ignore that selection and do not issue an invalid typed request. |

### 5. Good/Base/Bad Cases

- Good: selecting Usage in the sidebar sets its selected state, records a
  history entry, keeps the sidebar visible, and shows the typed Usage view.
- Good: activating the session usage ring uses a standard accessible Button and
  opens the same Usage route with a session filter; back/forward restores it.
- Good: the trend draws selectable green Token stacks, the annual heatmap maps
  larger daily totals to brighter semantic greens, and Models normalizes each
  day across at most ten stable categories.
- Base: a partial result shows known input Token, unknown output/total, and an
  exact-value/coverage tooltip in the table while retaining the real request
  count, without summary hover hints or a page-level status banner.
- Base: a new committed fact invalidates a hidden Usage view; the next activation
  refreshes once with the latest filters.
- Bad: coerce `usage` to `agent` or `management`, hide it inside Config Center,
  or lose `usage_session_id` during route history navigation.
- Bad: query SQLite directly from GPUI, inspect ACP payload fields in the view,
  render missing metrics as zero, or erase useful stale data on refresh failure.
- Bad: use a custom clickable ring without standard Button semantics and an
  ARIA label.

### 6. Tests Required

- Route serde/history tests cover `primary_tab = "usage"`, legacy decoding,
  session-filter round trip, back/forward, and branching after back.
- Shell/sidebar tests assert entry order, selected state, independent route
  application, sidebar visibility, and Agent-only panel suppression.
- Current-session control tests assert a standard Button, localized ARIA label,
  and navigation with the selected session filter.
- Controller tests assert request-generation race handling, hidden invalidation,
  retained last-success data, and unsupported/general error states.
- View-model/helper tests cover loading/empty/ready/unavailable, exact versus
  compact values, unknown/partial coverage, all five dimensions, all trend
  metrics, sorting, narrow summary column counts, the Token axis, stacked metric
  semantics, annual heatmap intensity/weekday alignment, model top-nine-plus-Other
  ranking, stable model colors, and both share bases.
- Cross-layer integration starts from fake ACP cumulative usage and asserts the
  resulting typed statistics displayed by the GPUI view model.

### 7. Wrong vs Correct

#### Wrong

```rust
div()
    .id("open-session-usage")
    .on_click(cx.listener(|this, _, _, cx| this.open_usage(None, cx)))
```

```rust
let displayed = metric.value.unwrap_or_default(); // Unknown becomes zero.
```

#### Correct

```rust
button_with_aria_label(
    Button::new("open-session-usage")
        .ghost()
        .on_click(cx.listener(|this, _, _, cx| {
            this.open_usage(this.selected_session_id.clone(), cx);
        })),
    locale::text("Open usage statistics", "...", "..."),
)
```

```rust
let displayed = metric
    .value
    .map(format_compact_number)
    .unwrap_or_else(|| locale::text("Unknown", "...", "...").to_string());
```
