use std::panic::{AssertUnwindSafe, catch_unwind};

use mathjax_svg_rs::{HorizontalAlign, Options};
use mermaid_rs_renderer::{RenderOptions, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineTheme {
    Light,
    Dark,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum EngineError {
    #[error("{engine} input exceeds its byte limit")]
    InputTooLarge { engine: &'static str },
    #[error("{engine} rejected an unsafe local include directive")]
    UnsafeDirective { engine: &'static str },
    #[error("{engine} failed: {message}")]
    Render {
        engine: &'static str,
        message: String,
    },
    #[error("{engine} panicked while rendering")]
    Panicked { engine: &'static str },
}

const MAX_ENGINE_SOURCE_BYTES: usize = 128 * 1024;

fn check_source(engine: &'static str, source: &str) -> Result<(), EngineError> {
    if source.len() > MAX_ENGINE_SOURCE_BYTES {
        return Err(EngineError::InputTooLarge { engine });
    }
    Ok(())
}

pub fn render_math(source: &str, font_size: f64) -> Result<String, EngineError> {
    const ENGINE: &str = "mathjax";
    check_source(ENGINE, source)?;
    let options = Options {
        font_size,
        horizontal_align: HorizontalAlign::Center,
    };
    catch_unwind(AssertUnwindSafe(|| {
        mathjax_svg_rs::render_tex(source, &options)
    }))
    .map_err(|_| EngineError::Panicked { engine: ENGINE })?
    .map_err(|message| EngineError::Render {
        engine: ENGINE,
        message,
    })
}

pub fn render_mermaid(source: &str, theme: EngineTheme) -> Result<String, EngineError> {
    const ENGINE: &str = "mermaid";
    check_source(ENGINE, source)?;
    let options = RenderOptions {
        theme: match theme {
            EngineTheme::Light => Theme::modern(),
            EngineTheme::Dark => Theme::dark(),
        },
        ..RenderOptions::default()
    };
    catch_unwind(AssertUnwindSafe(|| {
        mermaid_rs_renderer::render_with_options(source, options)
    }))
    .map_err(|_| EngineError::Panicked { engine: ENGINE })?
    .map_err(|error| EngineError::Render {
        engine: ENGINE,
        message: error.to_string(),
    })
}

pub fn render_plantuml(source: &str) -> Result<String, EngineError> {
    render_plantuml_with_theme(source, EngineTheme::Light)
}

pub fn render_plantuml_with_theme(source: &str, theme: EngineTheme) -> Result<String, EngineError> {
    const ENGINE: &str = "plantuml";
    check_source(ENGINE, source)?;
    if source.lines().any(|line| {
        let line = line.trim_start().to_ascii_lowercase();
        line.starts_with("!include") || line.starts_with("!import") || line.starts_with("!theme")
    }) {
        return Err(EngineError::UnsafeDirective { engine: ENGINE });
    }
    let mermaid = plantuml_to_mermaid(source)?;
    render_mermaid(&mermaid, theme).map_err(|error| EngineError::Render {
        engine: ENGINE,
        message: error.to_string(),
    })
}

fn plantuml_to_mermaid(source: &str) -> Result<String, EngineError> {
    const ENGINE: &str = "plantuml";
    let lines = source
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with('@')
                && !line.starts_with('\'')
                && !line.to_ascii_lowercase().starts_with("skinparam")
                && !line.to_ascii_lowercase().starts_with("hide ")
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return Err(EngineError::Render {
            engine: ENGINE,
            message: "empty PlantUML document".into(),
        });
    }

    if lines.iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("participant ")
            || lower.starts_with("actor ")
            || (line.contains(':') && contains_sequence_arrow(line))
    }) {
        return Ok(plantuml_sequence_to_mermaid(&lines));
    }
    if lines.iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("class ") || lower.starts_with("interface ") || lower.starts_with("enum ")
    }) {
        return Ok(format!("classDiagram\n{}", lines.join("\n")));
    }
    if lines
        .iter()
        .any(|line| line.starts_with("[*]") || line.to_ascii_lowercase().starts_with("state "))
    {
        return Ok(format!("stateDiagram-v2\n{}", lines.join("\n")));
    }
    if lines.iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("entity ") || line.contains("||--") || line.contains("}|--")
    }) {
        return Ok(plantuml_er_to_mermaid(&lines));
    }
    if lines.iter().any(|line| {
        matches!(line.to_ascii_lowercase().as_str(), "start" | "stop" | "end")
            || (line.starts_with(':') && line.ends_with(';'))
    }) {
        return Ok(plantuml_activity_to_mermaid(&lines));
    }
    if lines.iter().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("component ")
            || lower.starts_with("node ")
            || lower.starts_with("database ")
            || (line.contains('[') && contains_sequence_arrow(line))
    }) {
        return plantuml_component_to_mermaid(&lines);
    }

    Err(EngineError::Render {
        engine: ENGINE,
        message: "unsupported local PlantUML diagram family".into(),
    })
}

fn contains_sequence_arrow(line: &str) -> bool {
    ["->", "-->", "<-", "<--", "..>"]
        .iter()
        .any(|arrow| line.contains(arrow))
}

fn plantuml_sequence_to_mermaid(lines: &[&str]) -> String {
    use std::collections::BTreeSet;

    let mut participants = BTreeSet::new();
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("participant ") || lower.starts_with("actor ") {
            if let Some(name) = line.split_whitespace().nth(1) {
                participants.insert(name.trim_matches('"').to_string());
            }
        } else if let Some((edge, _)) = line.split_once(':')
            && let Some((left, right, _)) = sequence_edge(edge)
        {
            participants.insert(left.to_string());
            participants.insert(right.to_string());
        }
    }

    let mut output = String::from("sequenceDiagram\n");
    for participant in participants {
        output.push_str("participant ");
        output.push_str(&participant);
        output.push('\n');
    }
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("participant ") || lower.starts_with("actor ") {
            continue;
        }
        if let Some((edge, label)) = line.split_once(':')
            && let Some((left, right, arrow)) = sequence_edge(edge)
        {
            if arrow.starts_with('<') {
                output.push_str(right);
                output.push_str(if arrow.contains("--") { "-->>" } else { "->>" });
                output.push_str(left);
            } else {
                output.push_str(left);
                output.push_str(if arrow.contains("--") { "-->>" } else { "->>" });
                output.push_str(right);
            }
            output.push(':');
            output.push_str(label.trim());
            output.push('\n');
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    output
}

fn sequence_edge(edge: &str) -> Option<(&str, &str, &str)> {
    for arrow in ["<--", "-->", "<-", "->"] {
        if let Some((left, right)) = edge.split_once(arrow) {
            let left = left.trim();
            let right = right.trim();
            if !left.is_empty() && !right.is_empty() {
                return Some((left, right, arrow));
            }
        }
    }
    None
}

fn plantuml_er_to_mermaid(lines: &[&str]) -> String {
    let mut output = String::from("erDiagram\n");
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("entity ") {
            output.push_str(line.trim_start_matches(|character: char| character.is_alphabetic()));
            output.push('\n');
        } else if let Some((name, ty)) = line.trim_start_matches(['*', '+', '-']).split_once(':') {
            output.push_str("  ");
            output.push_str(ty.trim());
            output.push(' ');
            output.push_str(&safe_identifier(name));
            output.push('\n');
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    output
}

fn plantuml_activity_to_mermaid(lines: &[&str]) -> String {
    let mut output = String::from("flowchart TD\n");
    let mut previous = None::<String>;
    let mut index = 0usize;
    for line in lines {
        let lower = line.to_ascii_lowercase();
        let label = if lower == "start" {
            "Start".to_string()
        } else if matches!(lower.as_str(), "stop" | "end") {
            "End".to_string()
        } else if line.starts_with(':') && line.ends_with(';') {
            line.trim_matches([':', ';']).trim().to_string()
        } else if lower.starts_with("if ") {
            line.trim_start_matches("if").trim().to_string()
        } else {
            continue;
        };
        let node = format!("n{index}");
        if lower.starts_with("if ") {
            output.push_str(&format!("{node}{{\"{}\"}}\n", escape_mermaid_label(&label)));
        } else if matches!(lower.as_str(), "start" | "stop" | "end") {
            output.push_str(&format!("{node}([\"{}\"])\n", escape_mermaid_label(&label)));
        } else {
            output.push_str(&format!("{node}[\"{}\"]\n", escape_mermaid_label(&label)));
        }
        if let Some(previous) = previous {
            output.push_str(&format!("{previous} --> {node}\n"));
        }
        previous = Some(node);
        index += 1;
    }
    output
}

fn plantuml_component_to_mermaid(lines: &[&str]) -> Result<String, EngineError> {
    const ENGINE: &str = "plantuml";
    let mut output = String::from("flowchart LR\n");
    let mut emitted = 0usize;
    for line in lines {
        let Some((left, right)) = split_arrow(line) else {
            continue;
        };
        let left_label = component_label(left);
        let right_label = component_label(right);
        let left_id = safe_identifier(&left_label);
        let right_id = safe_identifier(&right_label);
        output.push_str(&format!(
            "{left_id}[\"{}\"] --> {right_id}[\"{}\"]\n",
            escape_mermaid_label(&left_label),
            escape_mermaid_label(&right_label)
        ));
        emitted += 1;
    }
    if emitted == 0 {
        return Err(EngineError::Render {
            engine: ENGINE,
            message: "component diagram has no supported relation".into(),
        });
    }
    Ok(output)
}

fn split_arrow(line: &str) -> Option<(&str, &str)> {
    for arrow in ["-->", "->", "..>"] {
        if let Some(parts) = line.split_once(arrow) {
            return Some(parts);
        }
    }
    None
}

fn component_label(value: &str) -> String {
    value
        .trim()
        .trim_matches(['[', ']', '"'])
        .split_once(" as ")
        .map_or_else(
            || value.trim().trim_matches(['[', ']', '"']),
            |(label, _)| label,
        )
        .trim_matches('"')
        .to_string()
}

fn safe_identifier(value: &str) -> String {
    let mut output = value
        .trim()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if output.is_empty() || output.starts_with(|character: char| character.is_ascii_digit()) {
        output.insert_str(0, "node_");
    }
    output
}

fn escape_mermaid_label(value: &str) -> String {
    value.replace('"', "&quot;").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_engine_smoke_matrix_produces_svg() {
        let math = render_math(r"\frac{a}{b}", 16.0).expect("MathJax fixture");
        assert!(math.contains("<svg"));

        for source in [
            "flowchart LR\nA[Start] --> B[Done]",
            "sequenceDiagram\nAlice->>Bob: Hello",
            "classDiagram\nclass Parser",
            "stateDiagram-v2\n[*] --> Ready",
            "erDiagram\nUSER ||--o{ NOTE : writes",
        ] {
            let svg = render_mermaid(source, EngineTheme::Light).expect(source);
            assert!(svg.contains("<svg"), "{source}");
        }

        for source in [
            "@startuml\nparticipant Alice\nAlice -> Bob: hello\n@enduml",
            "@startuml\nclass Parser\nParser <|-- MarkdownParser\n@enduml",
            "@startuml\nstate Ready\n[*] --> Ready\n@enduml",
            "@startuml\nstart\n:Parse input;\nstop\n@enduml",
            "@startuml\n[Parser] --> [Renderer]\n@enduml",
            "@startuml\nentity USER {\n* id : int\n}\nUSER ||--o{ NOTE : writes\n@enduml",
        ] {
            let svg = render_plantuml(source).expect(source);
            assert!(svg.contains("<svg"), "{source}");
        }
    }

    #[test]
    fn plantuml_remote_and_local_include_directives_are_rejected() {
        for directive in ["!include secret.puml", "!includeurl https://example.com/x"] {
            assert_eq!(
                render_plantuml(&format!("@startuml\n{directive}\n@enduml")),
                Err(EngineError::UnsafeDirective { engine: "plantuml" })
            );
        }
    }

    #[test]
    fn malformed_and_oversized_sources_return_typed_fallback_errors() {
        assert!(render_mermaid("this is not mermaid", EngineTheme::Light).is_err());
        assert!(render_plantuml("@startuml\nsalt\n{ unsupported }\n@enduml").is_err());
        assert_eq!(
            render_math(&"x".repeat(MAX_ENGINE_SOURCE_BYTES + 1), 16.0),
            Err(EngineError::InputTooLarge { engine: "mathjax" })
        );
    }

    #[test]
    fn mermaid_and_plantuml_outputs_follow_the_requested_theme() {
        let mermaid = "flowchart LR\nA-->B";
        assert_ne!(
            render_mermaid(mermaid, EngineTheme::Light).unwrap(),
            render_mermaid(mermaid, EngineTheme::Dark).unwrap()
        );
        let plantuml = "@startuml\nparticipant A\nA -> B: hello\n@enduml";
        assert_ne!(
            render_plantuml_with_theme(plantuml, EngineTheme::Light).unwrap(),
            render_plantuml_with_theme(plantuml, EngineTheme::Dark).unwrap()
        );
    }
}
