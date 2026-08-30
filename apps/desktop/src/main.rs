#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use gpui::{
    AnyView, App, AppContext as _, Bounds, WindowBounds, WindowDecorations, WindowOptions, px, size,
};
use gpui_component::{Root, TitleBar};
use vibex_agent::run_delegation_mcp_stdio;
use vibex_desktop::{
    DEFAULT_HEIGHT, DEFAULT_WIDTH, MIN_HEIGHT, MIN_WIDTH, app, assets,
    code_workbench::{CodeWorkbenchFixture, CodeWorkbenchFixtureKind},
    first_frame_probe, terminal_surface, theme,
};
use vibex_desktop_model::{AppearanceUiState, ThemeMode};
use vibex_terminal::run_terminal_feasibility;

mod acp_lifecycle;
mod composer_spike;
mod document_interaction;
mod native_content;
mod office_surface;
mod pdf_controller;
mod pdf_spike;
mod pdf_surface;
mod pdf_worker;

#[derive(Clone)]
enum LaunchMode {
    Workbench,
    AcpLifecycle(std::path::PathBuf),
    Composer(std::path::PathBuf),
    DocumentInteraction {
        library: std::path::PathBuf,
        pdf: std::path::PathBuf,
        office: std::path::PathBuf,
        output: std::path::PathBuf,
    },
    NativeContentWorkbench(Option<std::path::PathBuf>),
    CodeWorkbench(CodeWorkbenchFixtureKind),
    PdfWorkbench {
        library: std::path::PathBuf,
        document: std::path::PathBuf,
        output: Option<std::path::PathBuf>,
    },
}

fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.len() == 1 && arguments[0] == "--agent-delegation-mcp" {
        if let Err(error) = run_delegation_mcp_stdio() {
            eprintln!("Agent delegation MCP sidecar failed: {error}");
            std::process::exit(1);
        }
        return;
    }
    if arguments.iter().any(|argument| argument == "--probe") {
        println!(
            "{}",
            serde_json::to_string_pretty(&first_frame_probe())
                .expect("first-frame probe should serialize")
        );
        return;
    }
    if let [flag, output] = arguments.as_slice()
        && flag == "--spike-terminal"
    {
        let report = run_terminal_feasibility().unwrap_or_else(|error| {
            eprintln!("terminal feasibility probe failed: {error}");
            std::process::exit(1);
        });
        let serialized = serde_json::to_string_pretty(&report)
            .expect("terminal feasibility report should serialize");
        std::fs::write(output, format!("{serialized}\n")).unwrap_or_else(|error| {
            eprintln!("failed to write terminal feasibility report: {error}");
            std::process::exit(1);
        });
        return;
    }
    if let [flag, library, fixture, output, preview] = arguments.as_slice()
        && flag == "--spike-pdf"
    {
        let report =
            pdf_spike::run_pdf_feasibility(library, fixture, preview).unwrap_or_else(|error| {
                eprintln!("PDF feasibility probe failed: {error}");
                std::process::exit(1);
            });
        let serialized =
            serde_json::to_string_pretty(&report).expect("PDF feasibility report should serialize");
        std::fs::write(output, format!("{serialized}\n")).unwrap_or_else(|error| {
            eprintln!("failed to write PDF feasibility report: {error}");
            std::process::exit(1);
        });
        return;
    }
    if let [
        flag,
        library,
        document,
        generation,
        page_index,
        target_width,
        output_directory,
        report,
        fault_mode,
    ] = arguments.as_slice()
        && flag == "--native-content-pdf-worker-once"
    {
        let generation = generation.parse::<u64>().unwrap_or_else(|_| {
            eprintln!("PDF worker generation is invalid");
            std::process::exit(2);
        });
        let page_index = page_index.parse::<usize>().unwrap_or_else(|_| {
            eprintln!("PDF worker page index is invalid");
            std::process::exit(2);
        });
        let target_width = target_width.parse::<u16>().unwrap_or_else(|_| {
            eprintln!("PDF worker target width is invalid");
            std::process::exit(2);
        });
        pdf_worker::run_pdf_worker_once(
            library,
            document,
            generation,
            page_index,
            target_width,
            output_directory,
            report,
            fault_mode,
        )
        .unwrap_or_else(|error| {
            eprintln!("PDF worker failed: {error}");
            std::process::exit(1);
        });
        return;
    }
    if let [flag, library, fixture, output] = arguments.as_slice()
        && flag == "--native-content-pdf-worker-supervisor"
    {
        let report =
            pdf_worker::run_pdf_worker_supervisor(library, fixture).unwrap_or_else(|error| {
                eprintln!("PDF worker supervisor failed: {error}");
                std::process::exit(1);
            });
        let serialized = serde_json::to_string_pretty(&report)
            .expect("PDF worker supervisor report should serialize");
        std::fs::write(output, format!("{serialized}\n")).unwrap_or_else(|error| {
            eprintln!("failed to write PDF worker supervisor report: {error}");
            std::process::exit(1);
        });
        return;
    }
    if let [flag, library, fixture, output] = arguments.as_slice()
        && flag == "--native-content-pdf-worker-soak"
    {
        let report = pdf_worker::run_pdf_worker_soak(library, fixture).unwrap_or_else(|error| {
            eprintln!("PDF worker soak failed: {error}");
            std::process::exit(1);
        });
        let serialized =
            serde_json::to_string_pretty(&report).expect("PDF worker soak report should serialize");
        std::fs::write(output, format!("{serialized}\n")).unwrap_or_else(|error| {
            eprintln!("failed to write PDF worker soak report: {error}");
            std::process::exit(1);
        });
        return;
    }
    if let [
        flag,
        library,
        fixture,
        encrypted_fixture,
        too_many_pages_fixture,
        extreme_page_fixture,
        oversized_source,
        output,
    ] = arguments.as_slice()
        && flag == "--native-content-pdf-controller"
    {
        let encrypted_fixture_password = std::env::var("VIBEX_PDF_ENCRYPTED_FIXTURE_PASSWORD")
            .unwrap_or_else(|_| {
                eprintln!("PDF controller probe requires its reviewed fixture password");
                std::process::exit(1);
            });
        let report = pdf_controller::run_pdf_controller(
            library,
            fixture,
            encrypted_fixture,
            too_many_pages_fixture,
            extreme_page_fixture,
            oversized_source,
            &encrypted_fixture_password,
        )
        .unwrap_or_else(|error| {
            eprintln!("PDF controller probe failed: {error}");
            std::process::exit(1);
        });
        let serialized =
            serde_json::to_string_pretty(&report).expect("PDF controller report should serialize");
        std::fs::write(output, format!("{serialized}\n")).unwrap_or_else(|error| {
            eprintln!("failed to write PDF controller report: {error}");
            std::process::exit(1);
        });
        return;
    }
    if let [flag, output] = arguments.as_slice()
        && flag == "--native-content-contract"
    {
        native_content::write_native_content_contract(std::path::PathBuf::from(output))
            .unwrap_or_else(|error| {
                eprintln!("failed to write native content contract report: {error}");
                std::process::exit(1);
            });
        return;
    }
    if let [flag, output] = arguments.as_slice()
        && flag == "--native-content-switch-contract"
    {
        native_content::write_native_content_switch_contract(std::path::PathBuf::from(output))
            .unwrap_or_else(|error| {
                eprintln!("failed to write native content switch contract report: {error}");
                std::process::exit(1);
            });
        return;
    }

    let launch_mode = match arguments.as_slice() {
        [flag, output] if flag == "--spike-acp-lifecycle" => {
            LaunchMode::AcpLifecycle(std::path::PathBuf::from(output))
        }
        [flag, output] if flag == "--spike-composer" => {
            LaunchMode::Composer(std::path::PathBuf::from(output))
        }
        [flag, library, pdf, office, output] if flag == "--native-content-document-interaction" => {
            LaunchMode::DocumentInteraction {
                library: library.into(),
                pdf: pdf.into(),
                office: office.into(),
                output: output.into(),
            }
        }
        [flag] if flag == "--native-content-workbench" => LaunchMode::NativeContentWorkbench(None),
        [flag, output] if flag == "--native-content-workbench" => {
            LaunchMode::NativeContentWorkbench(Some(std::path::PathBuf::from(output)))
        }
        [flag, fixture] if flag == "--code-workbench-fixture" => {
            let kind = match fixture.as_str() {
                "files" => CodeWorkbenchFixtureKind::Files,
                "diff" => CodeWorkbenchFixtureKind::Diff,
                "markdown" => CodeWorkbenchFixtureKind::Markdown,
                _ => {
                    eprintln!("code workbench fixture must be files, diff, or markdown");
                    std::process::exit(2);
                }
            };
            LaunchMode::CodeWorkbench(kind)
        }
        [flag, library, document] if flag == "--native-content-pdf-workbench" => {
            LaunchMode::PdfWorkbench {
                library: std::path::PathBuf::from(library),
                document: std::path::PathBuf::from(document),
                output: None,
            }
        }
        [flag, library, document, output] if flag == "--native-content-pdf-workbench" => {
            LaunchMode::PdfWorkbench {
                library: std::path::PathBuf::from(library),
                document: std::path::PathBuf::from(document),
                output: Some(std::path::PathBuf::from(output)),
            }
        }
        [] => LaunchMode::Workbench,
        _ => {
            eprintln!(
                "usage: vibex-desktop [--probe|--code-workbench-fixture <files|diff|markdown>|--native-content-contract <output.json>|--native-content-switch-contract <output.json>|--native-content-document-interaction <pdfium-library> <fixture.pdf> <fixture.docx> <output.json>|--native-content-pdf-controller <pdfium-library> <fixture.pdf> <encrypted-fixture.pdf> <too-many-pages.pdf> <extreme-page.pdf> <oversized-source.pdf> <output.json>|--native-content-pdf-worker-once <pdfium-library> <fixture.pdf> <generation> <page-index> <target-width> <output-directory> <report.json> <none|crash|hang>|--native-content-pdf-worker-supervisor <pdfium-library> <fixture.pdf> <output.json>|--native-content-pdf-worker-soak <pdfium-library> <fixture.pdf> <output.json>|--native-content-pdf-workbench <pdfium-library> <fixture.pdf> [output.json]|--native-content-workbench [output.json]|--spike-acp-lifecycle <output.json>|--spike-composer <output.json>|--spike-terminal <output.json>|--spike-pdf <pdfium-library> <fixture.pdf> <output.json> <preview.rgba>]"
            );
            std::process::exit(2);
        }
    };

    let fixture_theme = if matches!(&launch_mode, LaunchMode::CodeWorkbench(_)) {
        match std::env::var("VIBEX_FIXTURE_THEME") {
            Ok(value) if value == "light" => Some(ThemeMode::Light),
            Ok(value) if value == "dark" => Some(ThemeMode::Dark),
            Ok(value) => {
                eprintln!("VIBEX_FIXTURE_THEME must be light or dark, got {value}");
                std::process::exit(2);
            }
            Err(std::env::VarError::NotPresent) => None,
            Err(error) => {
                eprintln!("failed to read VIBEX_FIXTURE_THEME: {error}");
                std::process::exit(2);
            }
        }
    } else {
        None
    };

    gpui_platform::application()
        .with_assets(assets::VibexAssets)
        .run(move |cx: &mut App| {
            gpui_tokio::init(cx);
            gpui_component::init(cx);
            if !matches!(launch_mode, LaunchMode::Workbench) {
                terminal_surface::bind_terminal_keys(cx);
            }
            if let Some(fixture_theme) = fixture_theme {
                let appearance = AppearanceUiState {
                    theme: fixture_theme,
                    ..AppearanceUiState::default()
                };
                theme::apply_appearance(&appearance, None, cx);
            }
            if let Err(error) = assets::load_fonts(cx) {
                eprintln!("{error}");
            }

            if matches!(launch_mode, LaunchMode::Workbench) {
                app::open_workbench_window(cx).unwrap_or_else(|error| {
                    eprintln!("{error}");
                    std::process::exit(1);
                });
                cx.activate(true);
                return;
            }

            let bounds = Bounds::centered(
                None,
                size(px(DEFAULT_WIDTH as f32), px(DEFAULT_HEIGHT as f32)),
                cx,
            );
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitleBar::title_bar_options()),
                app_id: Some("dev.vibex.desktop.preview".to_string()),
                icon: Some(assets::window_icon().unwrap_or_else(|error| {
                    eprintln!("{error}");
                    std::process::exit(1);
                })),
                window_min_size: Some(size(px(MIN_WIDTH as f32), px(MIN_HEIGHT as f32))),
                window_decorations: Some(WindowDecorations::Client),
                ..Default::default()
            };

            cx.open_window(options, |window, cx| {
                let view: AnyView = match launch_mode.clone() {
                    LaunchMode::AcpLifecycle(output) => cx
                        .new(|cx| acp_lifecycle::AcpLifecycleView::new(output, cx))
                        .into(),
                    LaunchMode::Composer(output) => cx
                        .new(|cx| composer_spike::ComposerSpikeView::new(output, window, cx))
                        .into(),
                    LaunchMode::DocumentInteraction {
                        library,
                        pdf,
                        office,
                        output,
                    } => cx
                        .new(|cx| {
                            document_interaction::DocumentInteractionWorkbench::new(
                                library, pdf, office, output, window, cx,
                            )
                        })
                        .into(),
                    LaunchMode::NativeContentWorkbench(output) => cx
                        .new(|cx| native_content::NativeContentWorkbench::new(output, window, cx))
                        .into(),
                    LaunchMode::CodeWorkbench(kind) => cx
                        .new(|cx| CodeWorkbenchFixture::new(kind, window, cx))
                        .into(),
                    LaunchMode::PdfWorkbench {
                        library,
                        document,
                        output,
                    } => cx
                        .new(|cx| {
                            pdf_surface::PdfSurface::new(
                                Ok(library),
                                Some(document),
                                output,
                                window,
                                cx,
                            )
                        })
                        .into(),
                    LaunchMode::Workbench => {
                        unreachable!("workbench launches through its app root")
                    }
                };
                cx.new(|cx| Root::new(view, window, cx).bordered(false))
            })
            .expect("failed to open Vibex preview window");
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn terminal_actions_are_registered_once(cx: &mut gpui::TestAppContext) {
        cx.update(terminal_surface::bind_terminal_keys);
    }
}
