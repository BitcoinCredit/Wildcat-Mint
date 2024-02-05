use serde::{Deserialize, Serialize};

///IMPORTANT!!!
#[derive(Debug, Serialize, Deserialize)]
pub struct GetMintQuery {
    pub amount: u64,
    pub pr: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PostMintQuery {
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Invoice {
    pub amount: u64,
    pub payment_request: String,
}

impl Invoice {
    pub fn new(amount: u64, payment_request: String) -> Self {
        Self {
            amount,
            payment_request,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateInvoiceResult {
    pub payment_hash: Vec<u8>,
    pub payment_request: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PayInvoiceResult {
    pub payment_hash: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateInvoiceParams {
    pub amount: u64,
    pub unit: String,
    pub memo: Option<String>,
    pub expiry: Option<u32>,
    pub webhook: Option<String>,
    pub internal: Option<bool>,
}
