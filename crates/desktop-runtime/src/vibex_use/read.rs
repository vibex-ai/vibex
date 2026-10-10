use super::*;
use vibex_core::ExecutionResultRange;

pub(super) fn bind_cursor(
    conn: &DbConnection,
    arguments: &serde_json::Value,
    cursor: &mut SessionReadCursor,
) -> VibexResult<()> {
    let session_id = session_reference(&cursor.session_ref)?;
    if let Some(reference) =
        parse_optional_ref(arguments, "executionRef", VibexUseResourceKind::Execution)?
    {
        if cursor
            .execution_ref
            .as_ref()
            .is_some_and(|old| old != &reference)
        {
            return Err(VibexError::validation(
                use_codes::CURSOR_SCOPE_MISMATCH,
                "cursor belongs to another execution",
            ));
        }
        cursor.execution_ref = Some(reference);
    }
    if let Some(reference) = cursor.execution_ref.as_ref() {
        let execution = reference
            .execution_id()
            .map(|id| VibexUseExecutionRepository::get(conn, &id))
            .transpose()?
            .flatten()
            .filter(|execution| execution.session_ref == cursor.session_ref)
            .ok_or_else(|| {
                VibexError::validation(
                    use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                    "execution not found",
                )
            })?;
        if !execution.is_settled() {
            return Err(VibexError::conflict(
                "vibex_use_execution_result_pending",
                "this execution has not produced a fixed result yet",
            ));
        }
        let range = execution
            .result_ranges
            .first()
            .cloned()
            .unwrap_or(ExecutionResultRange {
                start_sequence: 1,
                end_sequence: 0,
            });
        if cursor.range.as_ref().is_some_and(|current| {
            current.start_sequence < range.start_sequence
                || current.end_sequence > range.end_sequence
        }) {
            return Err(VibexError::validation(
                use_codes::CURSOR_SCOPE_MISMATCH,
                "cursor exceeds this execution's result range",
            ));
        }
        cursor.range.get_or_insert(range);
    }
    if let Some(reference) =
        parse_optional_ref(arguments, "resourceRef", VibexUseResourceKind::Resource)?
    {
        if cursor
            .resource_ref
            .as_ref()
            .is_some_and(|old| old != &reference)
        {
            return Err(VibexError::validation(
                use_codes::CURSOR_SCOPE_MISMATCH,
                "cursor belongs to another resource",
            ));
        }
        cursor.resource_ref = Some(reference);
    }
    if let Some(reference) = cursor.resource_ref.as_ref() {
        let (source, sequence) = reference
            .resource_location()
            .filter(|(source, _)| source == &session_id)
            .ok_or_else(|| {
                VibexError::validation(
                    use_codes::CURSOR_SCOPE_MISMATCH,
                    "resource belongs to another session",
                )
            })?;
        if cursor
            .range
            .as_ref()
            .is_some_and(|range| sequence < range.start_sequence || sequence > range.end_sequence)
        {
            return Err(VibexError::validation(
                use_codes::CURSOR_SCOPE_MISMATCH,
                "resource is outside the requested execution",
            ));
        }
        let _ = source;
        cursor.range = Some(ExecutionResultRange {
            start_sequence: sequence,
            end_sequence: sequence,
        });
    }
    if cursor.snapshot_end_sequence.is_none() {
        cursor.snapshot_end_sequence =
            Some(TimelineRepository::latest_sequence(conn, &session_id)?);
    }
    Ok(())
}

pub(super) fn session_read_page(
    conn: &DbConnection,
    session_ref: &VibexUseRef,
    mut cursor: SessionReadCursor,
    arguments: &serde_json::Value,
) -> VibexResult<SessionReadPage> {
    bind_cursor(conn, arguments, &mut cursor)?;
    let session_id = session_reference(session_ref)?;
    let max_items = bounded_usize(
        arguments,
        "maxItems",
        VIBEX_USE_DEFAULT_READ_ITEMS,
        VIBEX_USE_MAX_READ_ITEMS,
    );
    let max_chars = bounded_usize(
        arguments,
        "maxChars",
        VIBEX_USE_DEFAULT_READ_CHARS,
        VIBEX_USE_MAX_READ_CHARS,
    );
    let limit = u32::try_from(max_items.saturating_mul(4).clamp(1, 800)).unwrap_or(800);
    let snapshot = cursor.snapshot_end_sequence.unwrap_or_default();
    let lower = cursor
        .range
        .as_ref()
        .map_or(1, |range| range.start_sequence);
    let upper = cursor
        .range
        .as_ref()
        .map_or(snapshot, |range| range.end_sequence.min(snapshot));
    let forward = matches!(cursor.anchor, SessionReadAnchor::After { .. });
    let page = match cursor.anchor {
        SessionReadAnchor::After { sequence } => TimelineRepository::fetch_after(
            conn,
            &session_id,
            Some(sequence.max(lower.saturating_sub(1))),
            limit,
        )?,
        SessionReadAnchor::Before { sequence } => TimelineRepository::fetch_before(
            conn,
            &session_id,
            Some(sequence.min(upper.saturating_add(1))),
            limit,
        )?,
        SessionReadAnchor::Latest => TimelineRepository::fetch_before(
            conn,
            &session_id,
            Some(upper.saturating_add(1)),
            limit,
        )?,
    };
    let mut source: Vec<_> = page
        .items
        .iter()
        .filter(|item| item.sequence >= lower && item.sequence <= upper)
        .collect();
    if !forward {
        source.reverse();
    }
    let mut items = Vec::new();
    let mut budget = max_chars;
    let mut scanned = None;
    let mut next_offset = 0;
    let mut stopped = false;
    let mut truncated = false;
    for item in source {
        if items.len() == max_items || budget == 0 {
            stopped = true;
            break;
        }
        scanned = Some(item.sequence);
        if cursor.resource_ref.is_none() && !view_includes(cursor.view, item) {
            continue;
        }
        let text = if cursor.resource_ref.is_some() {
            serde_json::to_string(&item.payload).map_err(internal_encode_error)?
        } else {
            let Some(text) = item_text(item) else {
                continue;
            };
            text
        };
        if text.is_empty() {
            continue;
        }
        let offset = if items.is_empty() {
            cursor.character_offset
        } else {
            0
        };
        let total_chars = text.chars().count();
        if offset > total_chars {
            return Err(VibexError::conflict(
                "vibex_use_read_source_changed",
                "the content referenced by this cursor changed",
            ));
        }
        let chunk: String = text.chars().skip(offset).take(budget).collect();
        let consumed = chunk.chars().count();
        let cut = offset.saturating_add(consumed) < total_chars;
        budget = budget.saturating_sub(consumed);
        items.push(SessionReadEntry {
            sequence: item.sequence,
            kind: timeline_item_kind_name(item.kind).to_string(),
            source: timeline_source_name(item.source).to_string(),
            text: chunk,
            text_offset: offset,
            total_chars,
            truncated: cut,
            artifact_refs: vec![VibexUseRef::resource(&session_id, item.sequence)],
            timestamp_ms: item.timestamp_ms,
        });
        if cut {
            next_offset = offset + consumed;
            truncated = true;
            stopped = true;
            break;
        }
    }
    let database_more = scanned.is_some_and(|sequence| {
        if forward {
            page.has_newer && sequence < upper
        } else {
            page.has_older && sequence > lower
        }
    });
    let next_cursor = if stopped || database_more {
        scanned.map(|sequence| {
            let continuation = next_offset > 0;
            cursor.anchor = if forward {
                SessionReadAnchor::After {
                    sequence: if continuation {
                        sequence.saturating_sub(1)
                    } else {
                        sequence
                    },
                }
            } else {
                SessionReadAnchor::Before {
                    sequence: if continuation {
                        sequence.saturating_add(1)
                    } else {
                        sequence
                    },
                }
            };
            cursor.character_offset = next_offset;
            cursor.clone()
        })
    } else {
        None
    };
    items.sort_by_key(|entry| entry.sequence);
    Ok(SessionReadPage {
        session_ref: session_ref.clone(),
        view: cursor.view,
        items,
        snapshot_end_sequence: upper,
        has_more: next_cursor.is_some(),
        next_cursor,
        truncated,
        notices: if truncated {
            vec!["Continue with nextCursor to read the remaining characters.".to_string()]
        } else {
            Vec::new()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        AgentMessagePayload, AgentSessionSafety, TimelineRedactionState, TimelineSource,
        WorkspaceMode,
    };

    fn fixture() -> (DbConnection, VibexUseRef, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let mut conn = open_database(&directory.path().join("test.db")).unwrap();
        apply_migrations(&mut conn).unwrap();
        let (project, workspace) =
            WorkspaceRepository::ensure(&conn, directory.path(), WorkspaceMode::CurrentCheckout)
                .unwrap();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: "Reader".to_string(),
            project_id: project.id,
            workspace_id: workspace.id,
            workspace_root: workspace.root_path,
            workspace_mode: workspace.mode,
            agent_id: AgentId::parse("codex").unwrap(),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 1,
            updated_at_ms: 1,
            last_message_at_ms: 1,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(&conn, &session).unwrap();
        (conn, VibexUseRef::session(&session.id), directory)
    }

    fn append(conn: &mut DbConnection, reference: &VibexUseRef, text: String) {
        TimelineRepository::append(
            conn,
            &reference.session_id().unwrap(),
            TimelineSource::Agent,
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text,
                is_final: true,
            }),
            None,
            None,
            TimelineRedactionState::None,
        )
        .unwrap();
    }

    #[test]
    fn newest_pages_visit_every_entry_once_and_keep_their_snapshot() {
        let (mut conn, reference, _directory) = fixture();
        for sequence in 1..=200 {
            append(&mut conn, &reference, sequence.to_string());
        }
        let args = serde_json::json!({"maxItems": 50});
        let mut cursor = SessionReadCursor::new(
            reference.clone(),
            SessionReadView::Conversation,
            SessionReadAnchor::Latest,
        );
        let mut sequences = Vec::new();
        loop {
            let page = session_read_page(&conn, &reference, cursor, &args).unwrap();
            if sequences.is_empty() {
                assert_eq!(page.items.first().unwrap().sequence, 151);
                assert_eq!(page.items.last().unwrap().sequence, 200);
                append(&mut conn, &reference, "outside snapshot".to_string());
            }
            sequences.extend(page.items.iter().map(|item| item.sequence));
            match page.next_cursor {
                Some(next) => cursor = next,
                None => break,
            }
        }
        sequences.sort_unstable();
        assert_eq!(sequences, (1..=200).collect::<Vec<_>>());
    }

    #[test]
    fn a_long_unicode_entry_can_be_read_without_losing_characters() {
        let (mut conn, reference, _directory) = fixture();
        let original = "完整结果🦀abcdefghijklmnopqrstuvwxyz";
        append(&mut conn, &reference, original.to_string());
        let args = serde_json::json!({"maxChars": 4});
        let mut cursor = SessionReadCursor::new(
            reference.clone(),
            SessionReadView::Conversation,
            SessionReadAnchor::Latest,
        );
        let mut result = String::new();
        loop {
            let page = session_read_page(&conn, &reference, cursor, &args).unwrap();
            assert_eq!(page.items[0].text_offset, result.chars().count());
            result.push_str(&page.items[0].text);
            match page.next_cursor {
                Some(next) => cursor = next,
                None => break,
            }
        }
        assert_eq!(result, original);
    }

    #[test]
    fn explicit_context_bounds_never_include_neighbouring_messages() {
        let (mut conn, reference, _directory) = fixture();
        for sequence in 1..=12 {
            append(&mut conn, &reference, sequence.to_string());
        }
        let args = serde_json::json!({"fromSequence": 4, "throughSequence": 7});
        let cursor = parse_read_cursor(&conn, &args, &reference).unwrap();
        let page = session_read_page(&conn, &reference, cursor, &args).unwrap();
        assert_eq!(
            page.items
                .iter()
                .map(|item| item.sequence)
                .collect::<Vec<_>>(),
            vec![4, 5, 6, 7]
        );
        assert!(!page.has_more);
    }

    #[test]
    fn resource_reads_are_bound_to_the_originating_session_and_sequence() {
        let (mut conn, reference, _directory) = fixture();
        append(&mut conn, &reference, "complete payload".to_string());
        let resource = VibexUseRef::resource(&reference.session_id().unwrap(), 1);
        let args = serde_json::json!({"resourceRef": resource.as_uri()});
        let cursor = parse_read_cursor(&conn, &args, &reference).unwrap();
        let page = session_read_page(&conn, &reference, cursor, &args).unwrap();
        let payload: TimelinePayload = serde_json::from_str(&page.items[0].text).unwrap();
        assert!(
            matches!(payload, TimelinePayload::AgentMessage(message) if message.text == "complete payload")
        );
        let foreign = VibexUseRef::resource(&VibexSessionId::new(), 1);
        assert!(
            parse_read_cursor(
                &conn,
                &serde_json::json!({"resourceRef": foreign.as_uri()}),
                &reference
            )
            .is_err()
        );
    }
}
