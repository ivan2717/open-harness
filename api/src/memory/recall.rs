//! A small entry point into the complete archive, not a second task summary.
use agent_core::agent::RunEventEnvelope;
use agent_memory::{WikiState, WikiTaskKind, WikiTaskStatus};
use std::collections::BTreeSet;

const GUIDANCE: &str = include_str!("recall.txt");
pub(super) const MAX_RECALL_CHARS: usize = 2_400;

pub(super) fn render(state: &WikiState, events: &[RunEventEnvelope], limit: usize) -> String {
    let terms = query_terms(events);
    let mut text = String::new();
    if !append(&mut text, GUIDANCE.trim(), limit) {
        append(
            &mut text,
            "Historical Wiki: use session_memory index/search/read/tasks/source to verify omitted details.",
            limit,
        );
    }
    let progress = match state.source_cursor {
        Some(cursor) => format!(
            "Next unprocessed source: event #{}, character {}.",
            cursor.sequence, cursor.offset
        ),
        None => "Legacy notes: complete source ingestion is pending.".into(),
    };
    append(
        &mut text,
        &format!(
            "Wiki revision {}: {} pages, {} task records. {progress} Later evidence may be missing.",
            state.revision,
            state.pages.len(),
            state.tasks.len()
        ),
        limit,
    );

    // Deliberate, bounded redundancy protects requirements even if a rolling
    // summary omitted them. Never truncate a requirement's negation/conditions.
    let mut constraints: Vec<_> = state
        .tasks
        .iter()
        .filter(|item| {
            item.kind == WikiTaskKind::Constraint && item.status == WikiTaskStatus::Active
        })
        .collect();
    constraints.sort_by_key(|item| {
        std::cmp::Reverse((
            score(&terms, &item.text),
            item.evidence.iter().map(|proof| proof.sequence).max(),
        ))
    });
    let constraint_limit = (text.chars().count() + 900).min(limit.saturating_sub(250));
    let mut shown = Vec::new();
    let mut omitted_constraints = 0;
    for item in constraints {
        if append(&mut text, &record(item), constraint_limit) {
            shown.push(item.text.as_str());
        } else {
            omitted_constraints += 1;
        }
    }
    if omitted_constraints > 0 {
        append(
            &mut text,
            &format!(
                "{omitted_constraints} active constraints not shown; read session_memory tasks before acting."
            ),
            limit,
        );
    }

    // Keep current progress in the conversation/Compact handoff. Other records
    // earn prompt space only when relevant to the current topic.
    let mut records: Vec<_> = state
        .tasks
        .iter()
        .filter(|item| {
            item.status != WikiTaskStatus::Superseded
                && !(item.kind == WikiTaskKind::Constraint && item.status == WikiTaskStatus::Active)
                && score(&terms, &item.text) > 0
        })
        .collect();
    records.sort_by_key(|item| {
        std::cmp::Reverse((
            score(&terms, &item.text),
            matches!(
                item.status,
                WikiTaskStatus::Failed | WikiTaskStatus::Uncertain
            ),
            item.evidence.iter().map(|proof| proof.sequence).max(),
        ))
    });
    let record_limit = (text.chars().count() + 450).min(limit.saturating_sub(300));
    let mut shown_records = 0;
    for item in records {
        if !shown.contains(&item.text.as_str()) && append(&mut text, &record(item), record_limit) {
            shown.push(item.text.as_str());
            shown_records += 1;
            if shown_records == 2 {
                break;
            }
        }
    }

    let mut pages: Vec<_> = state
        .pages
        .iter()
        .map(|page| {
            (
                page,
                score(&terms, &page.title) * 3 + score(&terms, &page.content),
            )
        })
        .collect();
    pages.sort_by_key(|(page, relevance)| std::cmp::Reverse((*relevance, page.updated_through)));
    let mut listed = 0;
    for (page, _) in pages.iter().filter(|(_, relevance)| *relevance > 0).take(3) {
        let locator = format!(
            "Related page {} ({}) — session_memory read page_id={}",
            page.id, page.title, page.id
        );
        if !append(&mut text, &locator, limit) {
            continue;
        }
        listed += 1;
        // Use a matching complete paragraph, not an arbitrary prefix of a long
        // page. Large paragraphs remain available through the read tool.
        if let Some(passage) = matching_passage(&page.content, &terms, &shown, 450) {
            let snippet = format!(
                "Historical excerpt [events {:?}]: {passage}",
                page.source_sequences
            );
            if append(&mut text, &snippet, limit) {
                shown.push(passage);
            }
        }
    }
    if listed == 0 {
        // A topic change must not fill the prompt with unrelated page bodies.
        // A short directory keeps old evidence discoverable even without a hit.
        for (page, _) in pages.iter().take(3) {
            append(
                &mut text,
                &format!("Archive page {}: {}", page.id, page.title),
                limit,
            );
        }
    }

    // Old snapshots may contain only an overview. Keep that sole content
    // reachable without turning new overviews into a second progress summary.
    if state.pages.is_empty() && state.tasks.is_empty() && !state.overview.is_empty() {
        if let Some(passage) = matching_passage(&state.overview, &terms, &shown, 450) {
            append(
                &mut text,
                &format!("Legacy archive excerpt: {passage}"),
                limit,
            );
        } else {
            append(
                &mut text,
                "Legacy overview is available through session_memory index.",
                limit,
            );
        }
    }
    text
}

fn record(item: &agent_memory::WikiTaskItem) -> String {
    let sources: Vec<_> = item.evidence.iter().map(|source| source.sequence).collect();
    format!(
        "Task record: {}",
        serde_json::json!({
            "id":item.id,"kind":item.kind,"status":item.status,
            "text":item.text,"source_sequences":sources,
        })
    )
}

fn matching_passage<'a>(
    content: &'a str,
    terms: &BTreeSet<String>,
    shown: &[&str],
    max_chars: usize,
) -> Option<&'a str> {
    content
        .split("\n\n")
        .map(str::trim)
        .filter(|block| {
            !block.is_empty()
                && block.chars().count() <= max_chars
                && !shown.contains(block)
                && score(terms, block) > 0
        })
        .max_by_key(|block| score(terms, block))
}

fn append(output: &mut String, block: &str, limit: usize) -> bool {
    if output.chars().count() + block.chars().count() + 2 > limit {
        return false;
    }
    if !output.is_empty() {
        output.push_str("\n\n");
    }
    output.push_str(block);
    true
}

fn query_terms(events: &[RunEventEnvelope]) -> BTreeSet<String> {
    for query in events
        .iter()
        .rev()
        .filter_map(|event| event.event.user_input())
        .take(8)
    {
        let normalized = query
            .trim()
            .trim_end_matches(['。', '！', '!', '.', '?', '？'])
            .to_lowercase();
        if matches!(
            normalized.as_str(),
            "继续"
                | "继续吧"
                | "好的"
                | "好"
                | "嗯"
                | "接着做"
                | "继续执行"
                | "continue"
                | "go on"
                | "ok"
                | "okay"
                | "yes"
                | "proceed"
                | "do it"
        ) {
            continue;
        }
        let terms = terms(&query);
        if !terms.is_empty() {
            return terms;
        }
    }
    BTreeSet::new()
}

fn terms(query: &str) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    // Chinese character pairs avoid relying on whitespace tokenization; Latin
    // identifiers stay whole. Bound query work independently of history size.
    let query = query.chars().take(2_000).collect::<String>().to_lowercase();
    for word in query.split(|c: char| !c.is_alphanumeric()) {
        if matches!(
            word,
            "the"
                | "and"
                | "for"
                | "this"
                | "that"
                | "with"
                | "please"
                | "task"
                | "same"
                | "continue"
                | "go"
                | "on"
                | "ok"
                | "okay"
                | "yes"
                | "proceed"
        ) {
            continue;
        }
        if word.chars().count() > 1 {
            terms.insert(word.to_owned());
        }
        let chars: Vec<_> = word.chars().collect();
        for pair in chars
            .windows(2)
            .filter(|pair| pair.iter().all(|c| !c.is_ascii()))
        {
            terms.insert(pair.iter().collect());
        }
    }
    terms
}

fn score(terms: &BTreeSet<String>, text: &str) -> usize {
    let text = text.to_lowercase();
    terms
        .iter()
        .filter(|term| text.contains(term.as_str()))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retrieval_matches_chinese_topics_and_whole_latin_identifiers() {
        let query = terms("继续处理上下文压缩 tokenizer");
        assert!(score(&query, "上下文压缩决策") > score(&query, "文件导出"));
        assert!(score(&query, "tokenizer decision") > 0);
    }
}
