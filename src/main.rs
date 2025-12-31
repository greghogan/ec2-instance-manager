mod app;
mod config;
mod ec2;
mod pricing;
mod ui;

use anyhow::Result;
use app::{App, AppEvent, AppState};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
#[cfg(target_env = "musl")]
use mimalloc::MiMalloc;
use ratatui::{Terminal, backend::CrosstermBackend};
use std::{io, time::Duration};

// Validated that cleanup is necessary on error
struct Tui {
    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
}

impl Tui {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        let _ = self.terminal.show_cursor();
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Setup terminal with RAII guard
    let mut tui = Tui::new()?;

    // Create App
    // If this fails, tui dropped -> cleanup happens
    let mut app = App::new().await?;

    // Main loop
    let mut last_tick = std::time::Instant::now();
    loop {
        tui.terminal.draw(|f| ui::draw(f, &mut app))?;

        // Timer check
        if last_tick.elapsed()
            >= Duration::from_secs(app.config.refresh_interval_seconds.unwrap_or(5))
        {
            let client = app.ec2_client.clone();
            let tx = app.event_tx.clone();
            tokio::spawn(async move {
                match client.list_instances().await {
                    Ok(instances) => {
                        let _ = tx.send(AppEvent::InstancesFetched(instances)).await;
                    }
                    Err(e) => {
                        let _ = tx.send(AppEvent::Error(e.to_string())).await;
                    }
                }
            });
            last_tick = std::time::Instant::now();
        }

        // Check for async events
        // Check for async events
        if let Ok(event) = app.event_rx.try_recv() {
            match event {
                AppEvent::InstancesFetched(instances) => {
                    app.instances = instances;
                    app.update_filter();
                    app.last_refreshed = Some(chrono::Local::now().format("%H:%M:%S").to_string());
                }

                AppEvent::BulkOnDemandFetched(prices) => {
                    for (t, p) in prices {
                        let entry = app.type_prices.entry(t.clone()).or_insert(app::Prices {
                            on_demand: None,
                            spot: None,
                        });
                        entry.on_demand = Some(p);
                    }
                    if let AppState::SelectingType {
                        options,
                        selected_index,
                        default_mode_active,
                        ..
                    } = &mut app.state
                    {
                        // Only re-sort by price if the user has engaged with the input (default mode is NOT active).
                        // If default_mode_active is true, we keep the list sorted alphabetically to avoid confusing the user
                        // with a re-order while they are viewing the default suggestion.
                        if !*default_mode_active {
                            let type_prices = &app.type_prices;
                            options.sort_by(|a, b| {
                                let price_a = type_prices
                                    .get(a)
                                    .and_then(|p| p.on_demand)
                                    .unwrap_or(f64::MAX);
                                let price_b = type_prices
                                    .get(b)
                                    .and_then(|p| p.on_demand)
                                    .unwrap_or(f64::MAX);
                                price_a
                                    .partial_cmp(&price_b)
                                    .unwrap_or(std::cmp::Ordering::Equal)
                                    .then_with(|| a.cmp(b))
                            });
                            *selected_index = if options.is_empty() { None } else { Some(0) };
                        }
                    }
                }
                AppEvent::BulkSpotFetched(prices) => {
                    for (t, p) in prices {
                        let entry = app.type_prices.entry(t.clone()).or_insert(app::Prices {
                            on_demand: None,
                            spot: None,
                        });
                        entry.spot = Some(p);
                    }
                }
                AppEvent::InstancesUpdated => {
                    // Trigger background refresh
                    let client = app.ec2_client.clone();
                    let tx = app.event_tx.clone();
                    tokio::spawn(async move {
                        match client.list_instances().await {
                            Ok(instances) => {
                                let _ = tx.send(AppEvent::InstancesFetched(instances)).await;
                            }
                            Err(e) => {
                                let _ = tx.send(AppEvent::Error(e.to_string())).await;
                            }
                        }
                    });

                    // Reset to list if we were processing
                    if let AppState::Processing(_) = app.state {
                        app.state = AppState::List;
                    }
                }
                AppEvent::Error(e) => {
                    app.state = AppState::Processing(format!("Error: {}", e));
                    // User needs to Esc to clear
                }
                AppEvent::Message(msg) => {
                    app.state = AppState::Processing(msg);
                }
            }
        }

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                app.should_quit = true;
            }

            let current_state = app.state.clone();
            match current_state {
                AppState::List => {
                    match key.code {
                        KeyCode::Char('q') => app.should_quit = true,
                        KeyCode::Down => app.next(),
                        KeyCode::Up => app.previous(),
                        KeyCode::PageDown => {
                            let i = match app.list_state.selected() {
                                Some(i) => {
                                    (i + 20).min(app.filtered_instances.len().saturating_sub(1))
                                }
                                None => 0,
                            };
                            app.list_state.select(Some(i));
                        }
                        KeyCode::PageUp => {
                            let i = match app.list_state.selected() {
                                Some(i) => i.saturating_sub(20),
                                None => 0,
                            };
                            app.list_state.select(Some(i));
                        }
                        KeyCode::Home => app.list_state.select(Some(0)),
                        KeyCode::End => {
                            let i = app.filtered_instances.len().saturating_sub(1);
                            app.list_state.select(Some(i));
                        }
                        KeyCode::Char('f') => app.state = AppState::FilterInput,
                        KeyCode::Char('c') => {
                            if let Some(idx) = app.list_state.selected()
                                && let Some(instance) = app.filtered_instances.get(idx)
                            {
                                let mut initial_options =
                                    app.get_instance_types(instance.architecture.as_deref());
                                initial_options.sort();

                                // Trigger pricing fetch
                                if let Some(az) = &instance.availability_zone {
                                    let az = az.clone();
                                    let client = app.ec2_client.clone();
                                    let pricing_client = app.pricing_client.clone();
                                    let tx = app.event_tx.clone();

                                    if let Some(pc) = pricing_client
                                        && app.type_prices.is_empty()
                                    {
                                        let tx_od = tx.clone();
                                        tokio::spawn(async move {
                                            if let Ok(prices) =
                                                pc.fetch_all_on_demand_prices().await
                                            {
                                                let _ = tx_od
                                                    .send(AppEvent::BulkOnDemandFetched(prices))
                                                    .await;
                                            }
                                        });
                                    }

                                    let tx_spot = tx.clone();
                                    tokio::spawn(async move {
                                        if let Ok(prices) = client.fetch_all_spot_prices(&az).await
                                        {
                                            let _ = tx_spot
                                                .send(AppEvent::BulkSpotFetched(prices))
                                                .await;
                                        }
                                    });
                                }

                                let default_type =
                                    app.config.default_instance_type.clone().unwrap_or_default();
                                // Default: Sort by Name, Selection None
                                initial_options.sort();
                                let selected_index = None;

                                app.state = AppState::SelectingType {
                                    input: default_type,
                                    options: initial_options,
                                    selected_index,

                                    // Enable default mode:
                                    // 1. List is sorted by Name (not Cost).
                                    // 2. Async price updates will NOT re-sort the list.
                                    // 3. User interaction (typing/navigating) will disable this mode.
                                    default_mode_active: true,
                                };
                            }
                        }
                        KeyCode::Char('s') => {
                            if let Some(idx) = app.list_state.selected()
                                && let Some(instance) = app.filtered_instances.get(idx).cloned()
                            {
                                app.state =
                                    AppState::Processing(format!("Stopping {}...", instance.id));
                                let tx = app.event_tx.clone();
                                let client = app.ec2_client.clone();
                                let instance_id = instance.id.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = client.stop_instance(&instance_id).await {
                                        let _ = tx
                                            .send(AppEvent::Error(format!("Stop failed: {}", e)))
                                            .await;
                                    } else {
                                        let _ = tx
                                            .send(AppEvent::Message(
                                                "Stopped. Refreshing...".to_string(),
                                            ))
                                            .await;
                                        let _ = tx.send(AppEvent::InstancesUpdated).await;
                                    }
                                });
                            }
                        }
                        KeyCode::Char('S') => {
                            if let Some(idx) = app.list_state.selected()
                                && let Some(instance) = app.filtered_instances.get(idx).cloned()
                            {
                                app.state =
                                    AppState::Processing(format!("Starting {}...", instance.id));
                                let tx = app.event_tx.clone();
                                let client = app.ec2_client.clone();
                                let instance_id = instance.id.clone();

                                tokio::spawn(async move {
                                    if let Err(e) = client.start_instance(&instance_id).await {
                                        let _ = tx
                                            .send(AppEvent::Error(format!("Start failed: {}", e)))
                                            .await;
                                    } else {
                                        let _ = tx
                                            .send(AppEvent::Message(
                                                "Started. Refreshing...".to_string(),
                                            ))
                                            .await;
                                        let _ = tx.send(AppEvent::InstancesUpdated).await;
                                    }
                                });
                            }
                        }
                        KeyCode::Char('r') => {
                            if let Some(idx) = app.list_state.selected()
                                && let Some(instance) = app.filtered_instances.get(idx).cloned()
                            {
                                if instance.state.eq_ignore_ascii_case("running") {
                                    app.state = AppState::ConfirmReboot(instance.id.clone());
                                } else {
                                    app.state = AppState::Processing(format!(
                                        "Error: Cannot reboot instance {} because it is in state '{}'.",
                                        instance.id, instance.state
                                    ));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                AppState::ConfirmReboot(instance_id) => {
                    let instance_id = instance_id.clone();
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Enter => {
                            app.state =
                                AppState::Processing(format!("Rebooting {}...", instance_id));
                            let tx = app.event_tx.clone();
                            let client = app.ec2_client.clone();
                            tokio::spawn(async move {
                                if let Err(e) = client.reboot_instance(&instance_id).await {
                                    let _ = tx
                                        .send(AppEvent::Error(format!("Reboot failed: {}", e)))
                                        .await;
                                } else {
                                    let _ =
                                        tx.send(AppEvent::Message("Rebooted. (Note: AWS Runtime does not reset)".to_string())).await;
                                    let _ = tx.send(AppEvent::InstancesUpdated).await;
                                }
                            });
                        }
                        KeyCode::Char('f') => {
                            app.state = AppState::Processing(format!(
                                "Forced Rebooting {}...",
                                instance_id
                            ));
                            let tx = app.event_tx.clone();
                            let client = app.ec2_client.clone();
                            tokio::spawn(async move {
                                if let Err(e) = client.force_reboot_instance(&instance_id).await {
                                    let _ = tx
                                        .send(AppEvent::Error(format!(
                                            "Forced Reboot failed: {}",
                                            e
                                        )))
                                        .await;
                                } else {
                                    let _ = tx
                                        .send(AppEvent::Message(
                                            "Forced Reboot Complete.".to_string(),
                                        ))
                                        .await;
                                    let _ = tx.send(AppEvent::InstancesUpdated).await;
                                }
                            });
                        }
                        KeyCode::Char('n') | KeyCode::Esc => {
                            app.state = AppState::List;
                        }
                        _ => {}
                    }
                }
                AppState::FilterInput => match key.code {
                    KeyCode::Esc => {
                        app.state = AppState::List;
                    }
                    KeyCode::Enter => {
                        app.state = AppState::List;
                    }
                    KeyCode::Char(c) => {
                        app.filter.push(c);
                        app.update_filter();
                    }
                    KeyCode::Backspace => {
                        app.filter.pop();
                        app.update_filter();
                    }
                    _ => {}
                },
                AppState::SelectingType {
                    mut input,
                    options,
                    mut selected_index,
                    mut default_mode_active,
                } => {
                    match key.code {
                        KeyCode::Esc => {
                            app.state = AppState::List;
                        }
                        KeyCode::Up => {
                            if default_mode_active {
                                input.clear();
                                selected_index = Some(0);
                                default_mode_active = false;
                            } else if let Some(idx) = selected_index
                                && idx > 0
                            {
                                selected_index = Some(idx - 1);
                            }
                            app.state = AppState::SelectingType {
                                input,
                                options,
                                selected_index,
                                default_mode_active,
                            };
                        }
                        KeyCode::Down => {
                            if default_mode_active {
                                input.clear();
                                selected_index = Some(0);
                                default_mode_active = false;
                            } else if let Some(idx) = selected_index
                                && idx < options.len().saturating_sub(1)
                            {
                                selected_index = Some(idx + 1);
                            }
                            app.state = AppState::SelectingType {
                                input,
                                options,
                                selected_index,
                                default_mode_active,
                            };
                        }
                        KeyCode::PageUp => {
                            if default_mode_active {
                                input.clear();
                                selected_index = Some(0);
                                default_mode_active = false;
                            } else if let Some(idx) = selected_index {
                                let new_idx = idx.saturating_sub(20);
                                selected_index = Some(new_idx);
                            }
                            app.state = AppState::SelectingType {
                                input,
                                options,
                                selected_index,
                                default_mode_active,
                            };
                        }
                        KeyCode::PageDown => {
                            if default_mode_active {
                                input.clear();
                                selected_index = Some(0);
                                default_mode_active = false;
                            } else if let Some(idx) = selected_index {
                                let new_idx = (idx + 20).min(options.len().saturating_sub(1));
                                selected_index = Some(new_idx);
                            }
                            app.state = AppState::SelectingType {
                                input,
                                options,
                                selected_index,
                                default_mode_active,
                            };
                        }
                        KeyCode::Home => {
                            if default_mode_active {
                                input.clear();
                                default_mode_active = false;
                            }
                            selected_index = Some(0);
                            app.state = AppState::SelectingType {
                                input,
                                options,
                                selected_index,
                                default_mode_active,
                            };
                        }
                        KeyCode::End => {
                            if default_mode_active {
                                input.clear();
                                default_mode_active = false;
                            }
                            selected_index = Some(options.len().saturating_sub(1));
                            app.state = AppState::SelectingType {
                                input,
                                options,
                                selected_index,
                                default_mode_active,
                            };
                        }
                        KeyCode::Char(c) => {
                            if default_mode_active {
                                input.clear();
                                default_mode_active = false;
                            }
                            input.push(c);
                            let arch_string = app.get_selected_architecture();
                            let all_types = app.get_instance_types(arch_string.as_deref());

                            let mut new_options: Vec<String> = all_types
                                .into_iter()
                                .filter(|t| t.contains(input.as_str()))
                                .collect();

                            new_options.sort_by(|a, b| {
                                let price_a = app
                                    .type_prices
                                    .get(a)
                                    .and_then(|p| p.on_demand)
                                    .unwrap_or(f64::MAX);
                                let price_b = app
                                    .type_prices
                                    .get(b)
                                    .and_then(|p| p.on_demand)
                                    .unwrap_or(f64::MAX);
                                price_a
                                    .partial_cmp(&price_b)
                                    .unwrap_or(std::cmp::Ordering::Equal)
                                    .then_with(|| a.cmp(b))
                            });

                            let new_index = if new_options.is_empty() {
                                None
                            } else {
                                Some(0)
                            };
                            app.state = AppState::SelectingType {
                                input,
                                options: new_options,
                                selected_index: new_index,
                                default_mode_active,
                            };
                        }
                        KeyCode::Backspace => {
                            if !default_mode_active {
                                input.pop();
                                let arch_string = app.get_selected_architecture();
                                let mut all_types = app.get_instance_types(arch_string.as_deref());
                                let default_type =
                                    app.config.default_instance_type.as_deref().unwrap_or("");

                                if input.is_empty() {
                                    input = default_type.to_string();
                                    all_types.sort();
                                    app.state = AppState::SelectingType {
                                        input,
                                        options: all_types,
                                        selected_index: None,
                                        default_mode_active: true,
                                    };
                                } else {
                                    let mut new_options: Vec<String> = all_types
                                        .into_iter()
                                        .filter(|t| t.contains(input.as_str()))
                                        .collect();

                                    new_options.sort_by(|a, b| {
                                        let price_a = app
                                            .type_prices
                                            .get(a)
                                            .and_then(|p| p.on_demand)
                                            .unwrap_or(f64::MAX);
                                        let price_b = app
                                            .type_prices
                                            .get(b)
                                            .and_then(|p| p.on_demand)
                                            .unwrap_or(f64::MAX);
                                        price_a
                                            .partial_cmp(&price_b)
                                            .unwrap_or(std::cmp::Ordering::Equal)
                                            .then_with(|| a.cmp(b))
                                    });

                                    let new_index = if new_options.is_empty() {
                                        None
                                    } else {
                                        Some(0)
                                    };
                                    app.state = AppState::SelectingType {
                                        input,
                                        options: new_options,
                                        selected_index: new_index,
                                        default_mode_active,
                                    };
                                }
                            }
                        }
                        KeyCode::Enter => {
                            let selected_option = if options.len() == 1 {
                                options.first().cloned()
                            } else if let Some(i) = selected_index {
                                options.get(i).cloned()
                            } else if options.contains(&input) {
                                Some(input.clone())
                            } else {
                                None
                            };

                            if let Some(new_type) = selected_option
                                && let Some(idx) = app.list_state.selected()
                                && let Some(instance) = app.filtered_instances.get(idx).cloned()
                            {
                                if new_type == instance.instance_type {
                                    app.state = AppState::List;
                                } else {
                                    app.state = AppState::Processing(format!(
                                        "Stopping {}...",
                                        instance.id
                                    ));

                                    let client = app.ec2_client.clone();
                                    let tx = app.event_tx.clone();
                                    let instance_id = instance.id.clone();
                                    let credit_spec = app
                                        .config
                                        .t_family_credit
                                        .clone()
                                        .unwrap_or("standard".to_string());

                                    tokio::spawn(async move {
                                        // 1. Stop
                                        if let Err(e) = client.stop_instance(&instance_id).await {
                                            let _ = tx
                                                .send(AppEvent::Error(format!(
                                                    "Stop failed: {}",
                                                    e
                                                )))
                                                .await;
                                            return;
                                        }

                                        let _ = tx
                                            .send(AppEvent::Message(
                                                "Waiting for stop...".to_string(),
                                            ))
                                            .await;
                                        if let Err(e) = client
                                            .wait_until_stopped(
                                                &instance_id,
                                                std::time::Duration::from_secs(300),
                                            )
                                            .await
                                        {
                                            let _ = tx
                                                .send(AppEvent::Error(format!(
                                                    "Wait failed: {}",
                                                    e
                                                )))
                                                .await;
                                            return;
                                        }

                                        // 2. Modify
                                        let _ = tx
                                            .send(AppEvent::Message(format!(
                                                "Changing to {}...",
                                                new_type
                                            )))
                                            .await;
                                        if let Err(e) = client
                                            .modify_instance_type(&instance_id, &new_type)
                                            .await
                                        {
                                            let _ = tx
                                                .send(AppEvent::Error(format!(
                                                    "Modify failed: {:?}",
                                                    e
                                                )))
                                                .await;
                                            return;
                                        }

                                        // 3. T-credit
                                        if new_type.starts_with("t") {
                                            let _ = tx
                                                .send(AppEvent::Message(
                                                    "Setting credit spec...".to_string(),
                                                ))
                                                .await;
                                            if let Err(e) = client
                                                .modify_credit_specification(
                                                    &instance_id,
                                                    &credit_spec,
                                                )
                                                .await
                                            {
                                                let _ = tx
                                                    .send(AppEvent::Error(format!(
                                                        "Credit spec failed: {:?}",
                                                        e
                                                    )))
                                                    .await;
                                            }
                                        }

                                        // 4. Start
                                        let _ = tx
                                            .send(AppEvent::Message("Starting...".to_string()))
                                            .await;
                                        if let Err(e) = client.start_instance(&instance_id).await {
                                            let _ = tx
                                                .send(AppEvent::Error(format!(
                                                    "Start failed: {}",
                                                    e
                                                )))
                                                .await;
                                            return;
                                        }

                                        let _ = tx
                                            .send(AppEvent::Message(
                                                "Done. Refreshing...".to_string(),
                                            ))
                                            .await;
                                        let _ = tx.send(AppEvent::InstancesUpdated).await;
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                }
                AppState::Processing(_) => {
                    if key.code == KeyCode::Esc || key.code == KeyCode::Enter {
                        app.state = AppState::List;
                    }
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    // Restore terminal handled by Tui Drop

    // Save config on exit
    app.config.filter = Some(app.filter.clone());
    app.config.save().await?;

    Ok(())
}
