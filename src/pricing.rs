use anyhow::Result;
use aws_config::meta::region::RegionProviderChain;
use aws_sdk_pricing::Client;
use serde_json::Value;

#[derive(Clone)]
pub struct PricingClient {
    client: Client,
    region: String,
}

impl PricingClient {
    pub async fn new(region: &str) -> Result<Self> {
        // Pricing API is generally available in us-east-1 and ap-south-1.
        // We almost always want to query us-east-1 for the global price list.
        // However, we need to filter by the target "Location" (which is human readable region name).

        let region_provider = RegionProviderChain::default_provider().or_else("us-east-1");
        let _config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(region_provider)
            .load()
            .await;

        // We force the client to connect to us-east-1 for pricing API if not specified,
        // but actually standard AWS SDK behavior is fine if the user has a region set.
        // BUT, Price List API (GetProducts) endpoint is ONLY in us-east-1 and ap-south-1.
        // So we must override the region for the *client* to us-east-1.

        let pricing_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region("us-east-1")
            .load()
            .await;

        let client = Client::new(&pricing_config);

        Ok(Self {
            client,
            region: region.to_string(),
        })
    }

    pub async fn fetch_all_on_demand_prices(
        &self,
    ) -> Result<std::collections::HashMap<String, f64>> {
        let mut prices = std::collections::HashMap::new();
        let mut next_token = None;

        loop {
            let resp = self
                .client
                .get_products()
                .service_code("AmazonEC2")
                .filters(
                    aws_sdk_pricing::types::Filter::builder()
                        .field("regionCode")
                        .r#type(aws_sdk_pricing::types::FilterType::TermMatch)
                        .value(&self.region)
                        .build()?,
                )
                .filters(
                    aws_sdk_pricing::types::Filter::builder()
                        .field("tenancy")
                        .r#type(aws_sdk_pricing::types::FilterType::TermMatch)
                        .value("Shared")
                        .build()?,
                )
                .filters(
                    aws_sdk_pricing::types::Filter::builder()
                        .field("operatingSystem")
                        .r#type(aws_sdk_pricing::types::FilterType::TermMatch)
                        .value("Linux")
                        .build()?,
                )
                .filters(
                    aws_sdk_pricing::types::Filter::builder()
                        .field("preInstalledSw")
                        .r#type(aws_sdk_pricing::types::FilterType::TermMatch)
                        .value("NA")
                        .build()?,
                )
                .filters(
                    aws_sdk_pricing::types::Filter::builder()
                        .field("capacitystatus")
                        .r#type(aws_sdk_pricing::types::FilterType::TermMatch)
                        .value("Used")
                        .build()?,
                )
                .set_next_token(next_token.clone())
                .max_results(100)
                .send()
                .await?;

            if let Some(price_list) = resp.price_list {
                for item_json in price_list {
                    if let Ok(v) = serde_json::from_str::<Value>(&item_json)
                        && let Some(instance_type) = v
                            .get("product")
                            .and_then(|p| p.get("attributes"))
                            .and_then(|a| a.get("instanceType"))
                            .and_then(|s| s.as_str())
                        && let Some(terms) = v.get("terms").and_then(|t| t.get("OnDemand"))
                        && let Some(term) = terms.as_object().and_then(|o| o.values().next())
                        && let Some(price_dimensions) = term.get("priceDimensions")
                        && let Some(pd) =
                            price_dimensions.as_object().and_then(|o| o.values().next())
                        && let Some(price_per_unit) = pd.get("pricePerUnit")
                        && let Some(usd) = price_per_unit.get("USD")
                        && let Some(price_str) = usd.as_str()
                        && let Ok(p) = price_str.parse::<f64>()
                    {
                        prices.insert(instance_type.to_string(), p);
                    }
                }
            }

            next_token = resp.next_token;
            if next_token.is_none() {
                break;
            }
        }

        Ok(prices)
    }
}
