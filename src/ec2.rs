use anyhow::Result;
use aws_config::meta::region::RegionProviderChain;
use aws_sdk_ec2::Client;

#[derive(Debug, Clone)]
pub struct InstanceInfo {
    pub id: String,
    pub name: Option<String>,
    pub instance_type: String,
    pub state: String,
    pub architecture: Option<String>,
    pub availability_zone: Option<String>,
    pub public_ip: Option<String>,
    pub launch_time: Option<aws_sdk_ec2::primitives::DateTime>,
}

#[derive(Debug, Clone)]
pub struct InstanceTypeInfo {
    pub name: String,
    pub architectures: Vec<String>,
    pub vcpu: Option<i32>,
    pub memory_mib: Option<i64>,
}

#[derive(Clone)]
pub struct Ec2Client {
    client: Client,
}

impl Ec2Client {
    pub async fn new() -> Result<Self> {
        let region_provider = RegionProviderChain::default_provider().or_else("us-east-1");
        #[allow(deprecated)]
        let config = aws_config::from_env().region(region_provider).load().await;
        let client = Client::new(&config);
        Ok(Self { client })
    }

    #[allow(dead_code)]
    pub fn new_mock() -> Self {
        let config = aws_sdk_ec2::Config::builder()
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        let client = Client::from_conf(config);
        Self { client }
    }

    pub async fn list_instances(&self) -> Result<Vec<InstanceInfo>> {
        let resp = self.client.describe_instances().send().await?;
        let mut instances = Vec::new();

        for reservation in resp.reservations.unwrap_or_default() {
            for instance in reservation.instances.unwrap_or_default() {
                let id = instance.instance_id.unwrap_or_default();
                let state = instance
                    .state
                    .and_then(|s| s.name)
                    .map(|n| n.as_str().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                let instance_type = instance
                    .instance_type
                    .map(|t| t.as_str().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                let name = instance
                    .tags
                    .unwrap_or_default()
                    .iter()
                    .find(|t| t.key.as_deref() == Some("Name"))
                    .and_then(|t| t.value.clone());

                let architecture = instance.architecture.map(|a| a.as_str().to_string());

                let availability_zone = instance.placement.and_then(|p| p.availability_zone);

                let public_ip = instance.public_ip_address;

                instances.push(InstanceInfo {
                    id,
                    name,
                    instance_type,
                    state,
                    architecture,
                    availability_zone,
                    public_ip,
                    launch_time: instance.launch_time,
                });
            }
        }
        Ok(instances)
    }

    pub async fn stop_instance(&self, instance_id: &str) -> Result<()> {
        self.client
            .stop_instances()
            .instance_ids(instance_id)
            .send()
            .await?;
        Ok(())
    }

    pub async fn start_instance(&self, instance_id: &str) -> Result<()> {
        self.client
            .start_instances()
            .instance_ids(instance_id)
            .send()
            .await?;
        Ok(())
    }

    pub async fn reboot_instance(&self, instance_id: &str) -> Result<()> {
        self.client
            .reboot_instances()
            .instance_ids(instance_id)
            .send()
            .await?;
        Ok(())
    }

    pub async fn force_reboot_instance(&self, instance_id: &str) -> Result<()> {
        // Force stop
        self.client
            .stop_instances()
            .instance_ids(instance_id)
            .force(true)
            .send()
            .await?;

        // Wait for stopped state (5 minutes)
        self.wait_until_stopped(instance_id, std::time::Duration::from_secs(300))
            .await?;

        // Start
        self.client
            .start_instances()
            .instance_ids(instance_id)
            .send()
            .await?;

        Ok(())
    }

    pub async fn modify_instance_type(&self, instance_id: &str, new_type: &str) -> Result<()> {
        self.client
            .modify_instance_attribute()
            .instance_id(instance_id)
            .instance_type(
                aws_sdk_ec2::types::AttributeValue::builder()
                    .value(new_type)
                    .build(),
            )
            .send()
            .await?;
        Ok(())
    }

    pub async fn modify_credit_specification(
        &self,
        instance_id: &str,
        credit_spec: &str,
    ) -> Result<()> {
        self.client
            .modify_instance_credit_specification()
            .instance_credit_specifications(
                aws_sdk_ec2::types::InstanceCreditSpecificationRequest::builder()
                    .instance_id(instance_id)
                    .cpu_credits(credit_spec)
                    .build(),
            )
            .send()
            .await?;
        Ok(())
    }

    pub async fn wait_until_stopped(
        &self,
        instance_id: &str,
        timeout: std::time::Duration,
    ) -> Result<()> {
        let start_time = std::time::Instant::now();
        loop {
            let resp = self
                .client
                .describe_instances()
                .instance_ids(instance_id)
                .send()
                .await?;
            let state = resp
                .reservations
                .unwrap_or_default()
                .first()
                .and_then(|r| r.instances.as_ref())
                .and_then(|i| i.first())
                .and_then(|i| i.state.as_ref())
                .and_then(|s| s.name.as_ref())
                .map(|n| n.as_str().to_string())
                .unwrap_or_else(|| "unknown".to_string());

            if state == "stopped" {
                break;
            }

            if start_time.elapsed() > timeout {
                return Err(anyhow::anyhow!("Timeout waiting for instance to stop"));
            }

            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        Ok(())
    }

    pub async fn get_instance_types(&self) -> Result<Vec<InstanceTypeInfo>> {
        let mut types = Vec::new();
        let mut stream = self
            .client
            .describe_instance_types()
            .into_paginator()
            .send();

        while let Some(resp) = stream.next().await {
            let resp = resp?;
            for t in resp.instance_types.unwrap_or_default() {
                if let Some(name) = t.instance_type {
                    let name = name.as_str().to_string();
                    let architectures = t
                        .processor_info
                        .as_ref()
                        .and_then(|p| p.supported_architectures.as_ref())
                        .unwrap_or(&vec![])
                        .iter()
                        .map(|a| a.as_str().to_string())
                        .collect();

                    let vcpu = t.v_cpu_info.and_then(|v| v.default_v_cpus);
                    let memory_mib = t.memory_info.and_then(|m| m.size_in_mib);

                    types.push(InstanceTypeInfo {
                        name,
                        architectures,
                        vcpu,
                        memory_mib,
                    });
                }
            }
        }
        types.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(types)
    }

    pub async fn fetch_all_spot_prices(
        &self,
        az: &str,
    ) -> Result<std::collections::HashMap<String, f64>> {
        let now = aws_sdk_ec2::primitives::DateTime::from(std::time::SystemTime::now());
        let mut prices = std::collections::HashMap::new();

        let mut stream = self
            .client
            .describe_spot_price_history()
            .availability_zone(az)
            .product_descriptions("Linux/UNIX")
            .start_time(now)
            .into_paginator()
            .send();

        while let Some(resp) = stream.next().await {
            let resp = resp?;
            for entry in resp.spot_price_history.unwrap_or_default() {
                if let (Some(t), Some(p)) = (entry.instance_type, entry.spot_price)
                    && let Ok(price) = p.parse::<f64>()
                {
                    prices.insert(t.as_str().to_string(), price);
                }
            }
        }

        Ok(prices)
    }
}
