//! The gateway's Routes tab: the live route map, the story of one request,
//! and the request list that picks it.
//!
//! The three read as one: the map shows the latest requests while nothing is
//! picked, a row picked in the list pins the map to that request and replays
//! it, and the story under the map says in words what the map shows.

use super::route_stage::{
    FlightPhase, HopResult, RouteStage, StageAgent, StageFlight, StageHop, StageModel,
    StageTarget, TargetState, agent_hue,
};
use super::*;

/// How long a finished request keeps its place on the live map.
const LANDED_MS: i64 = 4_000;
/// How many Agents and members the live map draws at most.
const LIVE_AGENTS: usize = 6;
const LIVE_TARGETS: usize = 8;

/// What a story line is about, which decides its mark.
#[derive(Clone, Copy, PartialEq)]
enum StoryKind {
    Ask,
    Why,
    Skip,
    Failed,
    Answered,
    Broke,
    Waiting,
}

impl ManagementCenter {
    pub(super) fn render_workspace_gateway_routes(&mut self, cx: &mut Context<Self>) -> AnyElement {
        // A pinned request that has left the ledger and the live table
        // unpins: the map must not describe a request nobody can see.
        if let Some(pinned) = self.gateway_route_pinned.clone()
            && !self.gateway_requests.iter().any(|request| request.request_id == pinned)
            && !self.gateway_inflight.iter().any(|request| request.request_id == pinned)
        {
            self.gateway_route_pinned = None;
        }
        let model = self.workspace_route_stage_model(cx);
        let stage = self
            .gateway_route_stage
            .get_or_insert_with(|| cx.new(|_| RouteStage::new()))
            .clone();
        stage.update(cx, |stage, cx| stage.set_model(model, cx));

        let pinned = self.gateway_route_pinned.clone();
        let live_count = self.gateway_inflight.len();
        let status = match &pinned {
            Some(_) => h_flex()
                .items_center()
                .gap_2()
                .child(
                    Tag::color(ColorName::Blue)
                        .xsmall()
                        .child(management_locale_text("Replaying", "回放中", "回放中")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(management_locale_text(
                            "The map shows the request picked below.",
                            "路由图显示下方选中的请求。",
                            "路由圖顯示下方選中的請求。",
                        )),
                )
                .into_any_element(),
            None => h_flex()
                .items_center()
                .gap_2()
                .child(ws_state_fact(
                    management_locale_text("Live", "实时", "即時"),
                    cx.theme().success,
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(if live_count == 0 {
                            management_locale_text(
                                "Waiting for requests. Pick one below to replay its route.",
                                "等待请求中。点选下方任一请求即可回放它的路由。",
                                "等待請求中。點選下方任一請求即可回放它的路由。",
                            )
                            .to_string()
                        } else {
                            format!(
                                "{live_count} {}",
                                management_locale_text(
                                    "requests in flight",
                                    "个请求进行中",
                                    "個請求進行中"
                                )
                            )
                        }),
                )
                .into_any_element(),
        };
        let actions = h_flex()
            .items_center()
            .gap_2()
            .when(pinned.is_some(), |actions| {
                actions
                    .child(
                        Button::new("management-route-replay")
                            .small()
                            .ghost()
                            .icon(Icon::default().path("icons/vibex/rotate-ccw.svg"))
                            .label(management_locale_text("Replay", "重放", "重播"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(stage) = this.gateway_route_stage.clone() {
                                    stage.update(cx, |stage, cx| stage.replay(cx));
                                }
                            })),
                    )
                    .child(
                        Button::new("management-route-live")
                            .small()
                            .outline()
                            .label(management_locale_text("Back to live", "回到实时", "回到即時"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.gateway_route_pinned = None;
                                cx.notify();
                            })),
                    )
            })
            .into_any_element();

        let map = v_flex()
            .w_full()
            .min_w_0()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(theme::semantic_color("card", cx.theme().is_dark()).opacity(0.72))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_4()
                    .pt_3()
                    .child(status)
                    .child(actions),
            )
            .child(
                div()
                    .w_full()
                    .px_4()
                    .pb_2()
                    .child(
                        AnyView::from(stage)
                            .cached(StyleRefinement::default().w_full())
                            .into_any_element(),
                    ),
            )
            .child(self.render_workspace_route_story(cx));

        ws_stack(vec![map.into_any_element(), self.render_workspace_route_list(cx)])
    }

    /// The request the story tells: the picked one, else the newest.
    fn workspace_story_request(&self) -> Option<StoryRequest> {
        if let Some(pinned) = self.gateway_route_pinned.as_ref() {
            if let Some(live) = self
                .gateway_inflight
                .iter()
                .find(|request| &request.request_id == pinned)
            {
                return Some(StoryRequest::Live(live.clone()));
            }
            return self
                .gateway_requests
                .iter()
                .find(|request| &request.request_id == pinned)
                .cloned()
                .map(StoryRequest::Done);
        }
        if let Some(live) = self.gateway_inflight.last() {
            return Some(StoryRequest::Live(live.clone()));
        }
        self.gateway_requests
            .iter()
            .max_by_key(|request| request.created_at_ms)
            .cloned()
            .map(StoryRequest::Done)
    }

    fn render_workspace_route_story(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(request) = self.workspace_story_request() else {
            return div().into_any_element();
        };
        let lines = self.workspace_route_story(&request);
        let mut list = v_flex().w_full().min_w_0().gap_1p5();
        for (kind, text) in lines {
            let (mark, color) = match kind {
                StoryKind::Ask => ("icons/vibex/send.svg", cx.theme().foreground),
                StoryKind::Why => ("icons/vibex/git-branch.svg", cx.theme().muted_foreground),
                StoryKind::Skip => ("icons/vibex/chevrons-right.svg", cx.theme().muted_foreground),
                StoryKind::Failed => ("icons/vibex/rotate-ccw.svg", cx.theme().danger),
                StoryKind::Answered => ("icons/vibex/zap.svg", cx.theme().success),
                StoryKind::Broke => ("icons/vibex/shield-alert.svg", cx.theme().danger),
                StoryKind::Waiting => ("icons/vibex/clock.svg", cx.theme().info),
            };
            list = list.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_start()
                    .gap_2()
                    .child(
                        div().flex_none().pt(px(2.0)).child(
                            Icon::default().path(mark).size(px(13.0)).text_color(color),
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(if matches!(kind, StoryKind::Why | StoryKind::Skip) {
                                cx.theme().muted_foreground
                            } else {
                                cx.theme().foreground
                            })
                            .child(text),
                    ),
            );
        }
        let title = match &request {
            StoryRequest::Live(live) => format!(
                "{} · {}",
                gateway_clock(live.started_at_ms),
                management_locale_text("in flight", "进行中", "進行中")
            ),
            StoryRequest::Done(done) => gateway_clock(done.created_at_ms),
        };
        v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_xs().font_semibold().child(management_locale_text(
                        "How it was routed",
                        "路由经过",
                        "路由經過",
                    )))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(title),
                    ),
            )
            .child(list)
            .into_any_element()
    }

    /// The story of one request, one line per decision.
    fn workspace_route_story(&self, request: &StoryRequest) -> Vec<(StoryKind, String)> {
        let locale = locale::current_locale();
        let mut lines = Vec::new();
        match request {
            StoryRequest::Live(live) => {
                let agent = live
                    .agent_id
                    .as_deref()
                    .map(|agent| self.workspace_agent_label(agent))
                    .unwrap_or_else(|| {
                        management_locale_text("An Agent", "某个 Agent", "某個 Agent").to_string()
                    });
                lines.push((
                    StoryKind::Ask,
                    format!(
                        "{agent} {} {}",
                        management_locale_text("asked for", "请求了", "請求了"),
                        live.requested_model
                    ),
                ));
                if let Some(route) = live
                    .model_route_id
                    .as_deref()
                    .and_then(|id| self.model_routes.iter().find(|route| route.id.as_str() == id))
                {
                    lines.push((StoryKind::Why, self.workspace_route_group_line(route)));
                }
                let provider = self.workspace_provider_name(&live.model_provider_profile_id);
                let key = live
                    .credential_id
                    .as_ref()
                    .and_then(|id| self.workspace_key_name(id))
                    .map(|name| format!(" · {name}"))
                    .unwrap_or_default();
                if live.attempt > 1 {
                    lines.push((
                        StoryKind::Failed,
                        format!(
                            "{} {}",
                            live.attempt - 1,
                            management_locale_text(
                                "earlier attempts did not answer; the gateway moved on.",
                                "次尝试没有成功，网关已转到下一个成员。",
                                "次嘗試沒有成功，閘道已轉到下一個成員。",
                            )
                        ),
                    ));
                }
                let waited = (vibex_core::unix_timestamp_ms() - live.started_at_ms).max(0);
                lines.push((
                    StoryKind::Waiting,
                    if live.first_token_at_ms.is_some() {
                        format!(
                            "{provider}{key} {} · {}",
                            management_locale_text("is answering", "正在输出", "正在輸出"),
                            gateway_duration(waited)
                        )
                    } else {
                        format!(
                            "{} {provider}{key} · {}",
                            management_locale_text("Waiting for", "等待", "等待"),
                            gateway_duration(waited)
                        )
                    },
                ));
            }
            StoryRequest::Done(done) => {
                let agent = done
                    .agent_id
                    .as_deref()
                    .map(|agent| self.workspace_agent_label(agent))
                    .unwrap_or_else(|| {
                        management_locale_text("An Agent", "某个 Agent", "某個 Agent").to_string()
                    });
                lines.push((
                    StoryKind::Ask,
                    format!(
                        "{agent} {} {}",
                        management_locale_text("asked for", "请求了", "請求了"),
                        done.model_requested
                    ),
                ));
                if let Some(route) = self
                    .model_routes
                    .iter()
                    .find(|route| route.display_name == done.model_requested)
                {
                    lines.push((StoryKind::Why, self.workspace_route_group_line(route)));
                }
                if let Some(trace) = done.route.as_ref() {
                    if let Some(decision) = trace.decision.as_ref()
                        && !decision.is_unremarkable()
                    {
                        let mut parts = Vec::new();
                        if let Some(rule) = decision.rule_index {
                            parts.push(format!(
                                "{} {}",
                                management_locale_text("matched rule", "命中规则", "命中規則"),
                                rule + 1
                            ));
                        }
                        if let Some(intent) = decision.intent.as_deref() {
                            parts.push(format!(
                                "{} {intent}",
                                management_locale_text("intent", "意图", "意圖")
                            ));
                        }
                        if let Some(effort) = decision.effort.as_deref() {
                            parts.push(format!(
                                "{} {effort}",
                                management_locale_text("effort", "推理档位", "推理檔位")
                            ));
                        }
                        if decision.retries > 0 {
                            parts.push(format!(
                                "{} {}",
                                management_locale_text("whole walk retried", "整轮重试", "整輪重試"),
                                decision.retries
                            ));
                        }
                        if let Some(error) = decision.classifier_error.as_deref() {
                            parts.push(format!(
                                "{}: {error}",
                                management_locale_text(
                                    "classifier failed, default order used",
                                    "分类失败，按默认顺序",
                                    "分類失敗，按預設順序"
                                )
                            ));
                        }
                        if !parts.is_empty() {
                            lines.push((StoryKind::Why, parts.join(" · ")));
                        }
                    }
                    for skipped in &trace.skipped_members {
                        lines.push((
                            StoryKind::Skip,
                            format!(
                                "{} {} · {}",
                                management_locale_text("Skipped", "跳过", "跳過"),
                                skipped
                                    .provider_name
                                    .clone()
                                    .unwrap_or_else(|| self.workspace_provider_name(
                                        &skipped.model_provider_profile_id
                                    )),
                                gateway_failure_text(skipped.failure, locale)
                            ),
                        ));
                    }
                    if trace.skipped_cooling > 0 || trace.skipped_over_limit > 0 {
                        lines.push((
                            StoryKind::Skip,
                            format!(
                                "{} {} · {} {}",
                                management_locale_text("Stepped over", "跳过冷却中的 Key", "跳過冷卻中的 Key"),
                                trace.skipped_cooling,
                                management_locale_text("over their ceiling", "超出上限的 Key", "超出上限的 Key"),
                                trace.skipped_over_limit
                            ),
                        ));
                    }
                    for attempt in &trace.failed_attempts {
                        let provider = self
                            .workspace_provider_of_credential(&attempt.credential_id)
                            .map(|profile| profile.display_name.clone())
                            .unwrap_or_else(|| done.provider_name.clone());
                        let key = gateway_credential_name(attempt.credential_name.as_deref(), locale);
                        let rest = attempt
                            .failure
                            .cooldown_ms()
                            .map(|ms| {
                                format!(
                                    "，{} {}",
                                    management_locale_text("rests", "冷却", "冷卻"),
                                    gateway_duration(ms)
                                )
                            })
                            .unwrap_or_default();
                        lines.push((
                            StoryKind::Failed,
                            format!(
                                "{provider} · {key}：{}{rest}",
                                gateway_failure_text(attempt.failure, locale)
                            ),
                        ));
                    }
                }
                let key = gateway_credential_name(done.credential_name.as_deref(), locale);
                let ok = matches!(
                    done.outcome,
                    vibex_core::GatewayRequestOutcome::Ok | vibex_core::GatewayRequestOutcome::Failover
                );
                if ok {
                    let mut parts = vec![format!(
                        "{} · {key} {}",
                        done.provider_name,
                        management_locale_text("answered", "完成响应", "完成回應")
                    )];
                    if let Some(served) = done.model_served.as_deref()
                        && served != done.model_requested
                    {
                        parts.push(format!(
                            "{} {served}",
                            management_locale_text("as", "模型", "模型")
                        ));
                    }
                    if let Some(latency) = done.latency_ms {
                        parts.push(gateway_duration(latency));
                    }
                    parts.push(format!(
                        "{} {} / {} {}",
                        management_locale_text("in", "输入", "輸入"),
                        gateway_token_count(done.input_tokens),
                        management_locale_text("out", "输出", "輸出"),
                        gateway_token_count(done.output_tokens)
                    ));
                    if done.wire_in != done.wire_out {
                        parts.push(format!("{} → {}", done.wire_in, done.wire_out));
                    }
                    lines.push((StoryKind::Answered, parts.join(" · ")));
                } else {
                    let mut text = format!(
                        "{} · {}",
                        management_locale_text("The request failed", "请求失败", "請求失敗"),
                        done.status
                            .map(|status| format!("HTTP {status}"))
                            .unwrap_or_else(|| done.outcome.id().to_string())
                    );
                    if let Some(trace) = done.route.as_ref() {
                        if let Some(code) = trace.error_code.as_deref() {
                            text.push_str(&format!(" · {code}"));
                        }
                        if let Some(message) = trace.error_message.as_deref() {
                            let message: String = message.chars().take(240).collect();
                            text.push_str(&format!("：{message}"));
                        }
                    }
                    lines.push((StoryKind::Broke, text));
                }
            }
        }
        lines
    }

    fn workspace_route_group_line(&self, route: &vibex_core::ModelRoute) -> String {
        format!(
            "{} {} · {} · {} {}",
            management_locale_text("Route group", "路由组", "路由組"),
            route.display_name,
            management_route_strategy_label(route.strategy),
            route.members.iter().filter(|member| member.enabled).count(),
            management_locale_text("members", "个成员", "個成員")
        )
    }

    fn workspace_provider_name(&self, id: &vibex_core::ModelProviderProfileId) -> String {
        self.providers
            .iter()
            .find(|profile| &profile.id == id)
            .map(|profile| profile.display_name.clone())
            .unwrap_or_else(|| {
                management_locale_text("a removed Provider", "已删除的供应商", "已刪除的供應商")
                    .to_string()
            })
    }

    fn workspace_provider_of_credential(
        &self,
        id: &vibex_core::RequestId,
    ) -> Option<&vibex_core::ModelProviderProfile> {
        self.providers
            .iter()
            .find(|profile| profile.credentials.iter().any(|credential| &credential.id == id))
    }

    fn workspace_key_name(&self, id: &vibex_core::RequestId) -> Option<String> {
        self.providers
            .iter()
            .flat_map(|profile| profile.credentials.iter())
            .find(|credential| &credential.id == id)
            .map(|credential| {
                if credential.display_name.trim().is_empty() {
                    credential.masked_hint.clone()
                } else {
                    credential.display_name.clone()
                }
            })
    }

    // ----- the map's model -----------------------------------------------

    fn workspace_route_stage_model(&self, cx: &App) -> StageModel {
        let hub_title = gateway_display_name(&self.gateway.settings);
        let hub_running = self.gateway.runtime.status == vibex_core::ModelGatewayStatus::Running
            || self.gateway.runtime.status == vibex_core::ModelGatewayStatus::Degraded;
        let _ = cx;
        if let Some(pinned) = self.gateway_route_pinned.as_ref() {
            if let Some(done) = self
                .gateway_requests
                .iter()
                .find(|request| &request.request_id == pinned)
            {
                return self.workspace_pinned_model(done, hub_title, hub_running);
            }
        }
        self.workspace_live_model(hub_title, hub_running)
    }

    /// One recorded request, its every Key a row of its own.
    fn workspace_pinned_model(
        &self,
        done: &vibex_core::GatewayRequestSummary,
        hub_title: String,
        hub_running: bool,
    ) -> StageModel {
        let locale = locale::current_locale();
        let agent_id = done.agent_id.clone().unwrap_or_else(|| "agent".to_string());
        let agents = vec![StageAgent {
            label: self.workspace_agent_label(&agent_id),
            id: agent_id,
        }];
        let mut targets = Vec::<StageTarget>::new();
        let mut hops = Vec::new();
        let profile_of = |id: &vibex_core::ModelProviderProfileId| {
            self.providers.iter().find(|profile| &profile.id == id).cloned()
        };
        if let Some(trace) = done.route.as_ref() {
            for attempt in &trace.failed_attempts {
                let profile = self.workspace_provider_of_credential(&attempt.credential_id).cloned();
                targets.push(StageTarget {
                    title: profile
                        .as_ref()
                        .map(|profile| profile.display_name.clone())
                        .unwrap_or_else(|| done.provider_name.clone()),
                    subtitle: gateway_credential_name(attempt.credential_name.as_deref(), locale)
                        .to_string(),
                    profile,
                    state: TargetState::Ready,
                });
                hops.push(StageHop {
                    target: targets.len() - 1,
                    result: HopResult::Failed,
                    tag: SharedString::from(gateway_failure_text(attempt.failure, locale)),
                });
            }
        }
        let ok = matches!(
            done.outcome,
            vibex_core::GatewayRequestOutcome::Ok | vibex_core::GatewayRequestOutcome::Failover
        );
        targets.push(StageTarget {
            profile: profile_of(&done.model_provider_profile_id),
            title: done.provider_name.clone(),
            subtitle: format!(
                "{} · {}",
                gateway_credential_name(done.credential_name.as_deref(), locale),
                done.model_served.clone().unwrap_or_else(|| done.model_requested.clone())
            ),
            state: TargetState::Ready,
        });
        hops.push(StageHop {
            target: targets.len() - 1,
            result: if ok { HopResult::Ok } else { HopResult::Failed },
            tag: SharedString::from(if ok {
                done.latency_ms
                    .map(gateway_duration)
                    .unwrap_or_else(|| management_locale_text("answered", "已响应", "已回應").to_string())
            } else {
                done.status
                    .map(|status| format!("HTTP {status}"))
                    .unwrap_or_else(|| management_locale_text("failed", "失败", "失敗").to_string())
            }),
        });
        if let Some(trace) = done.route.as_ref() {
            for skipped in &trace.skipped_members {
                targets.push(StageTarget {
                    profile: profile_of(&skipped.model_provider_profile_id),
                    title: skipped
                        .provider_name
                        .clone()
                        .unwrap_or_else(|| self.workspace_provider_name(&skipped.model_provider_profile_id)),
                    subtitle: skipped.wire_protocol_id.clone(),
                    state: TargetState::Skipped(SharedString::from(gateway_failure_text(
                        skipped.failure,
                        locale,
                    ))),
                });
            }
        }
        StageModel {
            agents,
            targets,
            hub_title,
            hub_running,
            flights: vec![StageFlight {
                id: done.request_id.as_str().to_string(),
                agent: 0,
                hops,
                phase: FlightPhase::Replay,
            }],
            pinned: Some(done.request_id.as_str().to_string()),
        }
    }

    /// What is running now and what just finished, one row per Provider.
    fn workspace_live_model(&self, hub_title: String, hub_running: bool) -> StageModel {
        let now = vibex_core::unix_timestamp_ms();
        let live_ids = self
            .gateway_inflight
            .iter()
            .map(|request| request.request_id.clone())
            .collect::<BTreeSet<_>>();
        let landed = self
            .gateway_requests
            .iter()
            .filter(|request| !live_ids.contains(&request.request_id))
            .filter(|request| {
                let ended = request.created_at_ms + request.latency_ms.unwrap_or(0);
                now - ended < LANDED_MS && now >= ended
            })
            .collect::<Vec<_>>();

        // Agents: the ones with traffic first, then the ones bound to a group.
        let mut agent_ids = Vec::<String>::new();
        let mut push_agent = |id: &str, agents: &mut Vec<String>| {
            if !agents.iter().any(|existing| existing == id) {
                agents.push(id.to_string());
            }
        };
        for request in &self.gateway_inflight {
            push_agent(request.agent_id.as_deref().unwrap_or("agent"), &mut agent_ids);
        }
        for request in &landed {
            push_agent(request.agent_id.as_deref().unwrap_or("agent"), &mut agent_ids);
        }
        let gateway_routes = self
            .model_routes
            .iter()
            .filter(|route| route.kind.via_gateway())
            .collect::<Vec<_>>();
        for binding in &self.agent_model_route_bindings {
            if gateway_routes.iter().any(|route| route.id == binding.model_route_id) {
                push_agent(binding.agent_id.as_str(), &mut agent_ids);
            }
        }
        agent_ids.truncate(LIVE_AGENTS);

        // Members: the Providers with traffic first, then the groups' members.
        let mut provider_ids = Vec::<vibex_core::ModelProviderProfileId>::new();
        let mut push_provider = |id: &vibex_core::ModelProviderProfileId,
                                 providers: &mut Vec<vibex_core::ModelProviderProfileId>| {
            if !providers.contains(id) {
                providers.push(id.clone());
            }
        };
        for request in &self.gateway_inflight {
            push_provider(&request.model_provider_profile_id, &mut provider_ids);
        }
        for request in &landed {
            push_provider(&request.model_provider_profile_id, &mut provider_ids);
        }
        for route in &gateway_routes {
            for member in &route.members {
                push_provider(&member.model_provider_profile_id, &mut provider_ids);
            }
        }
        provider_ids.truncate(LIVE_TARGETS);

        let targets = provider_ids
            .iter()
            .map(|id| {
                let upstream = self
                    .gateway
                    .upstreams
                    .iter()
                    .find(|upstream| &upstream.model_provider_profile_id == id);
                let keys = upstream.map(|upstream| upstream.keys.as_slice()).unwrap_or_default();
                let ready = keys
                    .iter()
                    .filter(|key| key.state == vibex_core::GatewayKeyState::Ready)
                    .count();
                let cooling = keys
                    .iter()
                    .filter(|key| key.state == vibex_core::GatewayKeyState::Cooling)
                    .count();
                let state = if !keys.is_empty() && ready == 0 && cooling > 0 {
                    TargetState::Cooling(SharedString::from(management_locale_text(
                        "cooling",
                        "冷却中",
                        "冷卻中",
                    )))
                } else if !keys.is_empty() && ready == 0 {
                    TargetState::Invalid
                } else {
                    TargetState::Ready
                };
                StageTarget {
                    profile: self.providers.iter().find(|profile| &profile.id == id).cloned(),
                    title: self.workspace_provider_name(id),
                    subtitle: format!(
                        "{ready}/{} {}",
                        keys.len(),
                        management_locale_text("Keys ready", "个 Key 可用", "個 Key 可用")
                    ),
                    state,
                }
            })
            .collect::<Vec<_>>();

        let agent_index = |id: Option<&str>| {
            agent_ids
                .iter()
                .position(|agent| agent == id.unwrap_or("agent"))
        };
        let target_index = |id: &vibex_core::ModelProviderProfileId| {
            provider_ids.iter().position(|provider| provider == id)
        };
        let mut flights = Vec::new();
        for request in &self.gateway_inflight {
            let (Some(agent), Some(target)) = (
                agent_index(request.agent_id.as_deref()),
                target_index(&request.model_provider_profile_id),
            ) else {
                continue;
            };
            flights.push(StageFlight {
                id: request.request_id.as_str().to_string(),
                agent,
                hops: vec![StageHop {
                    target,
                    result: HopResult::Pending,
                    tag: SharedString::from(if request.attempt > 1 {
                        format!(
                            "{} {}",
                            management_locale_text("attempt", "第", "第"),
                            request.attempt
                        )
                    } else {
                        String::new()
                    }),
                }],
                phase: FlightPhase::Live {
                    started_ms: request.started_at_ms,
                    streaming: request.first_token_at_ms.is_some(),
                },
            });
        }
        for request in landed {
            let (Some(agent), Some(target)) = (
                agent_index(request.agent_id.as_deref()),
                target_index(&request.model_provider_profile_id),
            ) else {
                continue;
            };
            let ok = matches!(
                request.outcome,
                vibex_core::GatewayRequestOutcome::Ok | vibex_core::GatewayRequestOutcome::Failover
            );
            flights.push(StageFlight {
                id: request.request_id.as_str().to_string(),
                agent,
                hops: vec![StageHop {
                    target,
                    result: if ok { HopResult::Ok } else { HopResult::Failed },
                    tag: SharedString::from(if ok {
                        request
                            .latency_ms
                            .map(gateway_duration)
                            .unwrap_or_default()
                    } else {
                        request
                            .status
                            .map(|status| format!("HTTP {status}"))
                            .unwrap_or_else(|| {
                                management_locale_text("failed", "失败", "失敗").to_string()
                            })
                    }),
                }],
                phase: FlightPhase::Landed {
                    ended_ms: request.created_at_ms + request.latency_ms.unwrap_or(0),
                    ok,
                },
            });
        }
        StageModel {
            agents: agent_ids
                .into_iter()
                .map(|id| StageAgent {
                    label: if id == "agent" {
                        management_locale_text("Unknown Agent", "未知 Agent", "未知 Agent").to_string()
                    } else {
                        self.workspace_agent_label(&id)
                    },
                    id,
                })
                .collect(),
            targets,
            hub_title,
            hub_running,
            flights,
            pinned: None,
        }
    }

    // ----- the request list ----------------------------------------------

    fn render_workspace_route_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let pending = self.mutation.is_some();
        let query = self
            .gateway_request_search
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let active = self.gateway_request_filter;
        let center = cx.weak_entity();
        let filter = Button::new("management-gateway-request-filter")
            .small()
            .outline()
            .label(active.label())
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for filter in GatewayRequestFilter::ALL {
                    let center = center.clone();
                    menu = menu.item(
                        PopupMenuItem::new(filter.label())
                            .checked(filter == active)
                            .on_click(move |_, _, cx| {
                                center
                                    .update(cx, |this, cx| {
                                        this.gateway_request_filter = filter;
                                        cx.notify();
                                    })
                                    .ok();
                            }),
                    );
                }
                menu
            });
        let matches_query = |agent: Option<&str>, model: &str, provider: &str| {
            query.is_empty()
                || model.to_lowercase().contains(&query)
                || provider.to_lowercase().contains(&query)
                || agent.is_some_and(|agent| agent.to_lowercase().contains(&query))
        };
        let live = self
            .gateway_inflight
            .iter()
            .rev()
            .filter(|_| matches!(active, GatewayRequestFilter::All))
            .filter(|request| {
                matches_query(
                    request.agent_id.as_deref(),
                    &request.requested_model,
                    &self.workspace_provider_name(&request.model_provider_profile_id),
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut done = self
            .gateway_requests
            .iter()
            .filter(|request| {
                matches!(
                    (active, request.outcome),
                    (GatewayRequestFilter::All, _)
                        | (GatewayRequestFilter::Ok, vibex_core::GatewayRequestOutcome::Ok)
                        | (
                            GatewayRequestFilter::Failover,
                            vibex_core::GatewayRequestOutcome::Failover
                        )
                        | (
                            GatewayRequestFilter::Failed,
                            vibex_core::GatewayRequestOutcome::Failed
                                | vibex_core::GatewayRequestOutcome::Cooldown
                                | vibex_core::GatewayRequestOutcome::Rejected
                        )
                )
            })
            .filter(|request| {
                matches_query(
                    request.agent_id.as_deref(),
                    &request.model_requested,
                    &request.provider_name,
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        done.sort_by(|left, right| right.created_at_ms.cmp(&left.created_at_ms));

        let rows = live
            .into_iter()
            .map(RouteListRow::Live)
            .chain(done.into_iter().map(RouteListRow::Done))
            .collect::<Vec<_>>();
        let content = if rows.is_empty() {
            ws_empty_line(
                if self.gateway_requests.is_empty() && self.gateway_inflight.is_empty() {
                    management_locale_text(
                        "No request yet. Once an Agent goes through the gateway, each request is listed here without its prompt or reply.",
                        "还没有请求。Agent 经过网关后，每个请求都会列在这里，但不含提示词和回复。",
                        "還沒有請求。Agent 經過閘道後，每個請求都會列在這裡，但不含提示詞和回覆。",
                    )
                } else {
                    management_locale_text(
                        "No request matches. Clear the search or the filter.",
                        "没有匹配的请求，请清除搜索或筛选。",
                        "沒有符合的請求，請清除搜尋或篩選。",
                    )
                },
                cx,
            )
        } else {
            let height = (rows.len() as f32 * ROUTE_ROW_H).clamp(ROUTE_ROW_H * 3.0, 520.0);
            let sizes = Rc::new(
                rows.iter()
                    .map(|_| gpui_size(px(0.0), px(ROUTE_ROW_H)))
                    .collect::<Vec<GpuiSize<gpui::Pixels>>>(),
            );
            let rows = Rc::new(rows);
            div()
                .w_full()
                .h(px(height))
                .min_h_0()
                .child(v_virtual_list(
                    cx.entity(),
                    "management-gateway-request-virtual-list",
                    sizes,
                    move |this, range: Range<usize>, _, cx| {
                        range
                            .filter_map(|index| {
                                let row = rows.get(index)?;
                                Some(this.render_workspace_route_row(row, cx))
                            })
                            .collect()
                    },
                ))
                .into_any_element()
        };
        let foot = h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .justify_between()
            .gap_3()
            .pt_3()
            .child(
                div()
                    .min_w_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(management_locale_text(
                        "Metadata from the last 7 days is kept. It never contains the prompt, the reply, a Key, or image content.",
                        "保留最近 7 天的请求元数据，不含提示词、回复、Key 和图片内容。",
                        "保留最近 7 天的請求中繼資料，不含提示詞、回覆、Key 和圖片內容。",
                    )),
            )
            .when(!self.gateway_requests.is_empty(), |foot| {
                foot.child(
                    Button::new("management-gateway-clear-requests")
                        .small()
                        .ghost()
                        .label(management_locale_text("Clear records…", "清除记录…", "清除記錄…"))
                        .disabled(pending)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.confirm_clear_gateway_requests(window, cx)
                        })),
                )
            });
        ws_section(
            management_locale_text("Requests", "请求记录", "請求記錄"),
            Some(management_locale_text(
                "Select a request to see its route on the map above.",
                "选中一条请求，即可在上方路由图中查看它的路由。",
                "選取一筆請求，即可在上方路由圖中查看它的路由。",
            )),
            Some(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(rems(14.0))
                            .child(management_search_input_compact(&self.gateway_request_search, cx)),
                    )
                    .child(filter)
                    .into_any_element(),
            ),
            v_flex()
                .w_full()
                .child(route_list_header(cx))
                .child(content)
                .child(foot)
                .into_any_element(),
            cx,
        )
    }

    fn render_workspace_route_row(&self, row: &RouteListRow, cx: &mut Context<Self>) -> AnyElement {
        let locale = locale::current_locale();
        let (id, at_ms, agent, model, outcome, result, metrics) = match row {
            RouteListRow::Live(live) => {
                let provider = self.workspace_provider_name(&live.model_provider_profile_id);
                let waited = (vibex_core::unix_timestamp_ms() - live.started_at_ms).max(0);
                (
                    live.request_id.clone(),
                    live.started_at_ms,
                    live.agent_id.clone(),
                    live.requested_model.clone(),
                    RowOutcome::Live,
                    if live.attempt > 1 {
                        format!(
                            "{provider} · {} {}",
                            management_locale_text("attempt", "第", "第"),
                            live.attempt
                        )
                    } else {
                        provider
                    },
                    vec![gateway_duration(waited)],
                )
            }
            RouteListRow::Done(done) => {
                let failovers = done
                    .route
                    .as_ref()
                    .map(|route| route.failed_attempts.len())
                    .unwrap_or(0);
                let outcome = match done.outcome {
                    vibex_core::GatewayRequestOutcome::Ok if failovers == 0 => RowOutcome::Ok,
                    vibex_core::GatewayRequestOutcome::Ok
                    | vibex_core::GatewayRequestOutcome::Failover => RowOutcome::Moved(failovers.max(1)),
                    _ => RowOutcome::Failed,
                };
                let result = match outcome {
                    RowOutcome::Failed => format!(
                        "{} · {}",
                        done.provider_name,
                        done.status
                            .map(|status| format!("HTTP {status}"))
                            .or_else(|| done
                                .route
                                .as_ref()
                                .and_then(|route| route.error_code.clone()))
                            .unwrap_or_else(|| done.outcome.id().to_string())
                    ),
                    _ => format!(
                        "{} · {}",
                        done.provider_name,
                        done.model_served
                            .clone()
                            .unwrap_or_else(|| done.model_requested.clone())
                    ),
                };
                let mut metrics = Vec::new();
                if done.total_tokens > 0 {
                    metrics.push(format!("{} tok", gateway_token_count(done.total_tokens)));
                }
                if let Some(latency) = done.latency_ms {
                    metrics.push(gateway_duration(latency));
                }
                let _ = gateway_credential_name(done.credential_name.as_deref(), locale);
                (
                    done.request_id.clone(),
                    done.created_at_ms,
                    done.agent_id.clone(),
                    done.model_requested.clone(),
                    outcome,
                    result,
                    metrics,
                )
            }
        };
        let selected = self.gateway_route_pinned.as_ref() == Some(&id);
        let agent_key = agent.clone().unwrap_or_else(|| "agent".to_string());
        let hue = agent_hue(&agent_key, cx);
        let (dot, result_color) = match outcome {
            RowOutcome::Live => (cx.theme().info, cx.theme().foreground),
            RowOutcome::Ok => (cx.theme().success, cx.theme().foreground),
            RowOutcome::Moved(_) => (cx.theme().primary, cx.theme().foreground),
            RowOutcome::Failed => (cx.theme().danger, cx.theme().danger),
        };
        let pick = id.clone();
        h_flex()
            .id(SharedString::from(format!(
                "management-gateway-request-row-{}",
                id.as_str()
            )))
            .w_full()
            .h(px(ROUTE_ROW_H))
            .min_w_0()
            .items_center()
            .gap_3()
            .px_2()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(if selected {
                cx.theme().border
            } else {
                cx.theme().transparent
            })
            .when(selected, |row| row.bg(cx.theme().accent))
            .when(!selected, |row| row.hover(|style| style.bg(cx.theme().muted.opacity(0.6))))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.gateway_route_pinned = if this.gateway_route_pinned.as_ref() == Some(&pick) {
                    None
                } else {
                    Some(pick.clone())
                };
                cx.notify();
            }))
            .child(
                div()
                    .flex_none()
                    .w(rems(4.5))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(gateway_clock(at_ms)),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_1p5()
                    .child(div().flex_none().size(px(8.0)).rounded_full().bg(hue))
                    .child(agent_brand_icon(&agent_key, px(14.0), None))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(model),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("→"),
                    )
                    .child(div().flex_none().size(px(6.0)).rounded_full().bg(dot))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(result_color)
                            .child(result),
                    )
                    .when_some(
                        match outcome {
                            RowOutcome::Moved(count) => Some(count),
                            _ => None,
                        },
                        |line, count| {
                            line.child(
                                Tag::color(ColorName::Blue).xsmall().child(format!(
                                    "{} ×{count}",
                                    management_locale_text("moved", "转移", "轉移")
                                )),
                            )
                        },
                    )
                    .when(matches!(outcome, RowOutcome::Live), |line| {
                        line.child(
                            Tag::color(ColorName::Neutral)
                                .xsmall()
                                .child(management_locale_text("in flight", "进行中", "進行中")),
                        )
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .w(rems(8.0))
                    .text_right()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(metrics.join(" · ")),
            )
            .into_any_element()
    }
}

const ROUTE_ROW_H: f32 = 36.0;

#[derive(Clone)]
enum StoryRequest {
    Live(vibex_core::GatewayInflight),
    Done(vibex_core::GatewayRequestSummary),
}

#[derive(Clone)]
enum RouteListRow {
    Live(vibex_core::GatewayInflight),
    Done(vibex_core::GatewayRequestSummary),
}

#[derive(Clone, Copy)]
enum RowOutcome {
    Live,
    Ok,
    Moved(usize),
    Failed,
}

/// The list's column heads, on the same lanes as its rows.
fn route_list_header(cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .items_center()
        .gap_3()
        .px_2()
        .pb_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(div().flex_none().w(rems(4.5)).child(management_locale_text("Time", "时间", "時間")))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(management_locale_text("Agent · model asked", "Agent · 请求模型", "Agent · 請求模型")),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(management_locale_text("Answered by", "响应方", "回應方")),
        )
        .child(
            div()
                .flex_none()
                .w(rems(8.0))
                .text_right()
                .child(management_locale_text("Tokens · time", "Token · 用时", "Token · 用時")),
        )
        .into_any_element()
}

/// A wall-clock time as the list shows it.
fn gateway_clock(ms: i64) -> String {
    use chrono::TimeZone as _;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|time| time.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "—".to_string())
}

/// A duration as a reader says it: milliseconds under a second, then seconds,
/// then minutes.
fn gateway_duration(ms: i64) -> String {
    if ms < 1_000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        format!("{} min", ms / 60_000)
    }
}
