use crate::app::{App, AppState, Prices};
use crate::ec2::InstanceTypeInfo;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
};
use std::collections::HashMap;

pub fn draw(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(f.area());

    draw_list(f, app, chunks[0]);
    draw_statusbar(f, app, chunks[1]);

    if let AppState::SelectingType {
        input,
        options,
        selected_index,
        default_mode_active,
    } = &app.state
    {
        draw_selecting_type(
            f,
            &app.type_prices,
            &app.instance_type_map,
            input,
            options,
            *selected_index,
            *default_mode_active,
        );
    }

    if let AppState::ConfirmReboot(instance_id) = &app.state {
        draw_confirm_reboot(f, instance_id);
    }

    if let AppState::Processing(msg) = &app.state {
        draw_processing(f, msg);
    }
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let header_cells = [
        "Instance ID",
        "Name",
        "Type",
        "State",
        "AZ",
        "Public IP",
        "Arch",
        "Runtime",
    ]
    .iter()
    .map(|h| Cell::from(*h).style(Style::default().fg(Color::Yellow)));
    let header = Row::new(header_cells)
        .style(Style::default().add_modifier(Modifier::BOLD))
        .height(1)
        .bottom_margin(1);

    // Initial widths based on headers
    let mut w_id = 11;
    let mut w_name = 4;
    let mut w_type = 4;
    let mut w_state = 5;
    let mut w_az = 2;
    let mut w_ip = 9;
    let mut w_arch = 4;
    let mut w_uptime = 6;

    let rows: Vec<Row> = app
        .filtered_instances
        .iter()
        .map(|i| {
            let name = i.name.as_deref().unwrap_or("-");
            let az = i.availability_zone.as_deref().unwrap_or("-");
            let public_ip = i.public_ip.as_deref().unwrap_or("-");
            let arch = i.architecture.as_deref().unwrap_or("-");

            let uptime = if i.state.eq_ignore_ascii_case("running") {
                if let Some(launch_time) = i.launch_time {
                    let now = std::time::SystemTime::now();
                    if let Ok(duration) = now.duration_since(
                        std::time::SystemTime::try_from(launch_time)
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                    ) {
                        let secs = duration.as_secs();
                        let days = secs / 86400;
                        let hours = (secs % 86400) / 3600;
                        let mins = (secs % 3600) / 60;
                        if days > 0 {
                            format!("{}d {}h", days, hours)
                        } else if hours > 0 {
                            format!("{}h {}m", hours, mins)
                        } else {
                            format!("{}m", mins)
                        }
                    } else {
                        "-".to_string()
                    }
                } else {
                    "-".to_string()
                }
            } else {
                "-".to_string()
            };

            w_id = w_id.max(i.id.len());
            w_name = w_name.max(name.len());
            w_type = w_type.max(i.instance_type.len());
            w_state = w_state.max(i.state.len());
            w_az = w_az.max(az.len());
            w_ip = w_ip.max(public_ip.len());
            w_arch = w_arch.max(arch.len());
            w_uptime = w_uptime.max(uptime.len());

            let cells = vec![
                Cell::from(i.id.clone()),
                Cell::from(name.to_string()),
                Cell::from(i.instance_type.clone()).style(Style::default().fg(Color::Cyan)),
                Cell::from(i.state.clone()).style(if i.state == "running" {
                    Style::default().fg(Color::Green)
                } else if i.state == "stopped" {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default()
                }),
                Cell::from(az.to_string()),
                Cell::from(public_ip.to_string()),
                Cell::from(arch.to_string()),
                Cell::from(uptime),
            ];
            Row::new(cells).height(1)
        })
        .collect();

    let widths = [
        Constraint::Length(w_id as u16 + 2),
        Constraint::Length(w_name as u16 + 2),
        Constraint::Length(w_type as u16 + 2),
        Constraint::Length(w_state as u16 + 2),
        Constraint::Length(w_az as u16 + 2),
        Constraint::Length(w_ip as u16 + 2),
        Constraint::Length(w_arch as u16 + 2),
        Constraint::Length(w_uptime as u16 + 2),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("EC2 Instances"),
        )
        .row_highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .bg(Color::DarkGray),
        );

    f.render_stateful_widget(table, area, &mut app.list_state);
}

fn draw_statusbar(f: &mut Frame, app: &App, area: Rect) {
    let status_text = match &app.state {
        AppState::List => format!(
            "Filter: '{}' | q: Quit | f: Filter | c: Change Type | r: Reboot | \u{2191}\u{2193}: Select",
            app.filter
        ),
        AppState::FilterInput => format!("Filter: '{}' | Enter: Apply | Esc: Cancel", app.filter),
        AppState::SelectingType { .. } => {
            "Enter: Confirm | Esc: Cancel | Type to filter".to_string()
        }
        AppState::Processing(msg) => {
            if msg.starts_with("Error") {
                "Error Occurred | Enter/Esc: Dismiss".to_string()
            } else {
                "Processing...".to_string()
            }
        }
        AppState::ConfirmReboot(_) => {
            "y/Enter: Confirm Reboot | f: Force | n/Esc: Cancel".to_string()
        }
    };
    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(25)])
        .split(area);

    let p_status = Paragraph::new(status_text).style(Style::default().fg(Color::Gray));
    f.render_widget(p_status, layout[0]);

    if let Some(time) = &app.last_refreshed {
        let p_time = Paragraph::new(format!("Last Refreshed: {}", time))
            .alignment(Alignment::Right)
            .style(Style::default().fg(Color::Gray));
        f.render_widget(p_time, layout[1]);
    }
}

fn draw_selecting_type(
    f: &mut Frame,
    prices: &HashMap<String, Prices>,
    specs: &HashMap<String, InstanceTypeInfo>,
    input: &str,
    options: &[String],
    selected_index: Option<usize>,
    default_mode_active: bool,
) {
    let area = centered_rect(80, 60, f.area());
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    let style = if default_mode_active {
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC)
    } else {
        Style::default().fg(Color::Yellow)
    };

    let input_widget = Paragraph::new(input)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Select Instance Type"),
        )
        .style(style);
    f.render_widget(input_widget, chunks[0]);

    // Calculate widths from data
    let mut w_type = 4; // "Type"
    let mut w_cpu = 4; // "CPUs"
    let mut w_mem = 9; // "Mem (GiB)"
    let mut w_od = 8; // "OD Price"
    let mut w_spot = 10; // "Spot Price"
    let mut w_discount = 8; // "Discount"

    let data: Vec<_> = options
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let p = prices.get(s);
            let spec = specs.get(s);

            let od_val = p.and_then(|x| x.on_demand);
            let spot_val = p.and_then(|x| x.spot);

            let vcpu_str = spec
                .and_then(|t| t.vcpu)
                .map(|v| format!("{:>4}", v))
                .unwrap_or_else(|| "   -".to_string());
            let mem_str = spec
                .and_then(|t| t.memory_mib)
                .map(|m| format!("{:>8.1}", m as f64 / 1024.0))
                .unwrap_or_else(|| "       -".to_string());

            let od_str = od_val
                .map(|v| format!("${:.4}", v))
                .unwrap_or_else(|| "-".to_string());
            let spot_str = spot_val
                .map(|v| format!("${:.4}", v))
                .unwrap_or_else(|| "-".to_string());

            let discount_str = match (od_val, spot_val) {
                (Some(o), Some(s)) if o > 0.0 => format!("{:.0}%", (1.0 - (s / o)) * 100.0),
                _ => "-".to_string(),
            };

            w_type = w_type.max(s.len());
            w_cpu = w_cpu.max(vcpu_str.len());
            w_mem = w_mem.max(mem_str.len());
            w_od = w_od.max(od_str.len());
            w_spot = w_spot.max(spot_str.len());
            w_discount = w_discount.max(discount_str.len());

            (i, s, vcpu_str, mem_str, od_str, spot_str, discount_str)
        })
        .collect();

    let rows: Vec<Row> = data
        .into_iter()
        .map(|(_, s, v, m, o, sp, r)| Row::new(vec![s.clone(), v, m, o, sp, r]))
        .collect();

    let constraints = [
        Constraint::Length(w_type as u16 + 2),
        Constraint::Length(w_cpu as u16 + 2),
        Constraint::Length(w_mem as u16 + 2),
        Constraint::Length(w_od as u16 + 2),
        Constraint::Length(w_spot as u16 + 2),
        Constraint::Length(w_discount as u16 + 2),
    ];

    let table = Table::new(rows, constraints)
        .header(
            Row::new(vec![
                "Type",
                "CPUs",
                "Mem (GiB)",
                "OD Price",
                "Spot Price",
                "Discount",
            ])
            .style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .bottom_margin(1),
        )
        .block(Block::default().borders(Borders::ALL).title("Suggestions"))
        .row_highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );

    let mut state = TableState::default();
    state.select(selected_index);
    f.render_stateful_widget(table, chunks[1], &mut state);
}

fn draw_processing(f: &mut Frame, msg: &str) {
    let area = centered_rect(60, 20, f.area());
    let p = Paragraph::new(msg)
        .block(Block::default().borders(Borders::ALL).title("Processing"))
        .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
        .alignment(ratatui::layout::Alignment::Center)
        .wrap(ratatui::widgets::Wrap { trim: true });
    f.render_widget(p, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn draw_confirm_reboot(f: &mut Frame, instance_id: &str) {
    let area = centered_rect(50, 20, f.area());
    let block = Block::default()
        .title("Confirm Reboot")
        .borders(Borders::ALL);
    let text = format!("Are you sure you want to reboot {}? (y/n)", instance_id);
    let p = Paragraph::new(text)
        .block(block)
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD));
    f.render_widget(p, area);
}
