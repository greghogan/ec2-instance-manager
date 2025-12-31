use crate::config::AppConfig;
use crate::ec2::{Ec2Client, InstanceInfo, InstanceTypeInfo};
use anyhow::Result;
use ratatui::widgets::TableState;
use tokio::sync::mpsc;

#[derive(Clone, Debug, PartialEq)]
pub enum AppState {
    List,
    FilterInput,
    SelectingType {
        input: String,
        options: Vec<String>,
        selected_index: Option<usize>,
        default_mode_active: bool,
    }, // Input buffer and list of visible options
    ConfirmReboot(String), // Instance ID
    Processing(String),    // Message to show
}

pub enum AppEvent {
    Error(String),
    Message(String),
    InstancesUpdated,
    InstancesFetched(Vec<InstanceInfo>),

    BulkOnDemandFetched(std::collections::HashMap<String, f64>),
    BulkSpotFetched(std::collections::HashMap<String, f64>),
}

#[derive(Debug, Clone)]
pub struct Prices {
    pub on_demand: Option<f64>,
    pub spot: Option<f64>,
}

pub struct App {
    pub should_quit: bool,
    pub instances: Vec<InstanceInfo>,
    pub filtered_instances: Vec<InstanceInfo>,
    pub instance_types: Vec<InstanceTypeInfo>,
    pub list_state: TableState,
    pub ec2_client: Ec2Client,
    pub pricing_client: Option<crate::pricing::PricingClient>,
    pub config: AppConfig,
    pub filter: String,
    pub state: AppState,
    pub event_tx: mpsc::Sender<AppEvent>,
    pub event_rx: mpsc::Receiver<AppEvent>,
    pub type_prices: std::collections::HashMap<String, Prices>,
    pub instance_type_map: std::collections::HashMap<String, InstanceTypeInfo>,
    pub last_refreshed: Option<String>,
}

impl App {
    pub async fn new() -> Result<Self> {
        let config = AppConfig::load().await?;
        let ec2_client = Ec2Client::new().await?;
        // Fetch instances and types concurrently or sequentially
        let instances = ec2_client.list_instances().await?;
        let instance_types = ec2_client
            .get_instance_types()
            .await
            .unwrap_or_else(|_| vec![]);
        let mut instance_type_map = std::collections::HashMap::new();
        for t in &instance_types {
            instance_type_map.insert(t.name.clone(), t.clone());
        }
        let mut list_state = TableState::default();
        list_state.select(Some(0));

        // Init PricingClient
        let region_provider =
            aws_config::meta::region::RegionProviderChain::default_provider().or_else("us-east-1");
        let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(region_provider)
            .load()
            .await;
        let region = aws_config
            .region()
            .map(|r| r.as_ref())
            .unwrap_or("us-east-1");

        let pricing_client = crate::pricing::PricingClient::new(region).await.ok();

        let (event_tx, event_rx) = mpsc::channel(10);

        let mut app = Self {
            should_quit: false,
            instances: instances.clone(),
            filtered_instances: instances,
            instance_types,
            list_state,
            ec2_client,
            pricing_client,
            filter: config.filter.clone().unwrap_or_default(),
            config,
            state: AppState::List,
            event_tx,
            event_rx,
            type_prices: std::collections::HashMap::new(),
            instance_type_map,
            last_refreshed: Some(chrono::Local::now().format("%H:%M:%S").to_string()),
        };
        app.update_filter();
        Ok(app)
    }

    pub fn next(&mut self) {
        let i = match self.list_state.selected() {
            Some(i) => {
                if i >= self.filtered_instances.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    pub fn previous(&mut self) {
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.filtered_instances.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    /// Updates the `filtered_instances` list based on the current `filter` string.
    /// Matches against: ID, Name (tag), Instance Type, and State.
    /// Search is case-insensitive.
    pub fn update_filter(&mut self) {
        if self.filter.is_empty() {
            self.filtered_instances = self.instances.clone();
        } else {
            let f = self.filter.to_lowercase();
            self.filtered_instances = self
                .instances
                .iter()
                .filter(|i| {
                    i.id.to_lowercase().contains(&f)
                        || i.name
                            .as_ref()
                            .map(|n| n.to_lowercase().contains(&f))
                            .unwrap_or(false)
                        || i.instance_type.to_lowercase().contains(&f)
                        || i.state.to_lowercase().contains(&f)
                })
                .cloned()
                .collect();
        }
        // Reset selection if out of bounds or empty
        if self.filtered_instances.is_empty() {
            self.list_state.select(None);
        } else {
            self.filtered_instances.sort_by(|a, b| {
                let name_a = a.name.as_deref().unwrap_or("");
                let name_b = b.name.as_deref().unwrap_or("");
                name_a.cmp(name_b)
            });

            let sel = self.list_state.selected().unwrap_or(0);
            if sel >= self.filtered_instances.len() {
                self.list_state.select(Some(0));
            }
        }
    }

    pub fn get_instance_types(&self, architecture: Option<&str>) -> Vec<String> {
        let arch = architecture.unwrap_or("x86_64"); // Default to x86_64 if unknown

        if self.instance_types.is_empty() {
            // Fallback if empty (e.g. API failed) - assume these are x86_64
            vec![
                "t3.nano",
                "t3.micro",
                "t3.small",
                "t3.medium",
                "t3.large",
                "t3a.nano",
                "t3a.micro",
                "t3a.small",
                "t3a.medium",
                "t2.nano",
                "t2.micro",
                "t2.small",
                "m5.large",
                "m5.xlarge",
                "c5.large",
            ]
            .into_iter()
            .map(|s| s.to_string())
            .collect()
        } else {
            self.instance_types
                .iter()
                .filter(|t| t.architectures.iter().any(|a| a == arch))
                .map(|t| t.name.clone())
                .collect()
        }
    }

    pub fn get_selected_architecture(&self) -> Option<String> {
        self.list_state
            .selected()
            .and_then(|i| self.filtered_instances.get(i))
            .and_then(|inst| inst.architecture.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec2::Ec2Client;
    use tokio::sync::mpsc;

    fn create_mock_instance(id: &str, name: Option<&str>, state: &str) -> InstanceInfo {
        InstanceInfo {
            id: id.to_string(),
            name: name.map(|n| n.to_string()),
            instance_type: "t3.micro".to_string(),
            state: state.to_string(),
            architecture: Some("x86_64".to_string()),
            availability_zone: Some("us-east-1a".to_string()),
            public_ip: None,
            launch_time: None,
        }
    }

    fn create_test_app(instances: Vec<InstanceInfo>) -> App {
        let (tx, rx) = mpsc::channel(100);
        let ec2_client = Ec2Client::new_mock();

        App {
            should_quit: false,
            instances: instances.clone(),
            filtered_instances: instances,
            instance_types: vec![],
            list_state: TableState::default(),
            ec2_client,
            pricing_client: None,
            config: AppConfig {
                default_instance_type: None,
                refresh_interval_seconds: Some(5),
                filter: None,
                t_family_credit: None,
            },
            filter: String::new(),
            state: AppState::List,
            event_tx: tx,
            event_rx: rx,
            type_prices: std::collections::HashMap::new(),
            instance_type_map: std::collections::HashMap::new(),
            last_refreshed: None,
        }
    }

    #[test]
    fn test_filter_empty() {
        let instances = vec![
            create_mock_instance("i-1", Some("Foo"), "running"),
            create_mock_instance("i-2", Some("Bar"), "stopped"),
        ];
        let mut app = create_test_app(instances);

        app.filter = String::new();
        app.update_filter();

        assert_eq!(app.filtered_instances.len(), 2);
    }

    #[test]
    fn test_filter_by_id() {
        let instances = vec![
            create_mock_instance("i-123", Some("Foo"), "running"),
            create_mock_instance("i-456", Some("Bar"), "stopped"),
        ];
        let mut app = create_test_app(instances);

        app.filter = "123".to_string();
        app.update_filter();

        assert_eq!(app.filtered_instances.len(), 1);
        assert_eq!(app.filtered_instances[0].id, "i-123");
    }

    #[test]
    fn test_filter_by_name() {
        let instances = vec![
            create_mock_instance("i-1", Some("WebServer"), "running"),
            create_mock_instance("i-2", Some("DB"), "stopped"),
        ];
        let mut app = create_test_app(instances);

        app.filter = "Web".to_string();
        app.update_filter();

        assert_eq!(app.filtered_instances.len(), 1);
        assert_eq!(app.filtered_instances[0].name.as_deref(), Some("WebServer"));
    }

    #[test]
    fn test_filter_by_state() {
        let instances = vec![
            create_mock_instance("i-1", None, "running"),
            create_mock_instance("i-2", None, "stopped"),
        ];
        let mut app = create_test_app(instances);

        app.filter = "stopped".to_string();
        app.update_filter();

        assert_eq!(app.filtered_instances.len(), 1);
        assert_eq!(app.filtered_instances[0].state, "stopped");
    }

    #[test]
    fn test_filter_case_insensitive() {
        let instances = vec![create_mock_instance("i-1", Some("TEST"), "running")];
        let mut app = create_test_app(instances);

        app.filter = "test".to_string();
        app.update_filter();
        assert_eq!(app.filtered_instances.len(), 1);

        app.filter = "RUNNING".to_string();
        app.update_filter();
        assert_eq!(app.filtered_instances.len(), 1);
    }
}
