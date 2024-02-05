use std::{collections::HashSet, sync::Arc};

use moksha_core::{
    crypto,
    dhke::Dhke,
    model::{
        split_amount, BlindedMessage, BlindedSignature, MintKeyset, PostSplitResponse, Proofs,
        TotalAmount,
    },
};

use crate::{
    database::Database,
    error::MokshaMintError,
    info::MintInfoSettings,
    lightning::{Lightning, LightningType},
    model::Invoice,
    MintBuilder,
};

#[derive(Clone)]
pub struct Mint {
    pub lightning: Arc<dyn Lightning + Send + Sync>,
    pub lightning_type: LightningType,
    pub keyset: MintKeyset,
    pub db: Arc<dyn Database + Send + Sync>,
    pub dhke: Dhke,
    pub lightning_fee_config: LightningFeeConfig,
    pub mint_info: MintInfoSettings,
}

#[derive(Clone, Debug)]
pub struct LightningFeeConfig {
    pub fee_percent: f32,
    pub fee_reserve_min: u64,
    // TODO check if fee_percent is in range
}

impl LightningFeeConfig {
    pub fn new(fee_percent: f32, fee_reserve_min: u64) -> Self {
        Self {
            fee_percent,
            fee_reserve_min,
        }
    }
}

impl Default for LightningFeeConfig {
    fn default() -> Self {
        Self {
            fee_percent: 1.0,
            fee_reserve_min: 4000,
        }
    }
}

impl Mint {
    pub fn new(
        secret: String,
        derivation_path: String,
        lightning: Arc<dyn Lightning + Send + Sync>,
        lightning_type: LightningType,
        db: Arc<dyn Database + Send + Sync>,
        lightning_fee_config: LightningFeeConfig,
        mint_info: MintInfoSettings,
    ) -> Self {
        Self {
            lightning,
            lightning_type,
            lightning_fee_config,
            keyset: MintKeyset::new(secret, derivation_path),
            db,
            dhke: Dhke::new(),
            mint_info,
        }
    }

    pub fn builder() -> MintBuilder {
        MintBuilder::new()
    }

    pub fn fee_reserve(&self, amount_msat: u64) -> u64 {
        let fee_percent = self.lightning_fee_config.fee_percent as f64 / 100.0;
        let fee_reserve = (amount_msat as f64 * fee_percent) as u64;
        std::cmp::max(fee_reserve, self.lightning_fee_config.fee_reserve_min)
    }

    pub fn create_blinded_signatures(
        &self,
        blinded_messages: &[BlindedMessage],
    ) -> Result<Vec<BlindedSignature>, MokshaMintError> {
        let promises = blinded_messages
            .iter()
            .map(|blinded_msg| {
                let private_key = self.keyset.private_keys.get(&blinded_msg.amount).unwrap(); // FIXME unwrap
                let blinded_sig = self.dhke.step2_bob(blinded_msg.b_, private_key).unwrap(); // FIXME unwrap
                BlindedSignature {
                    id: Some(self.keyset.keyset_id.clone()),
                    amount: blinded_msg.amount,
                    c_: blinded_sig,
                }
            })
            .collect::<Vec<BlindedSignature>>();
        Ok(promises)
    }

    ///IMPORTANT!!!
    pub async fn create_invoice(&self, amount: u64, key: String) -> Result<(String, String), MokshaMintError> {
        // let pr = self.lightning.create_invoice(amount).await?.payment_request;
        //TODO generate hash
        // let key = crypto::generate_hash();
        self.db
            .add_pending_invoice(key.clone(), Invoice::new(amount, key.clone()))?;
        Ok((key.clone(), key.clone()))
    }

    ///IMPORTANT!!!
    pub async fn mint_tokens(
        &self,
        invoice_hash: String,
        outputs: &[BlindedMessage],
    ) -> Result<Vec<BlindedSignature>, MokshaMintError> {
        let invoice = self.db.get_pending_invoice(invoice_hash.clone())?;

        // let is_paid = self
        //     .lightning
        //     .is_invoice_paid(invoice.payment_request.clone())
        //     .await?;
        //
        // if !is_paid {
        //     return Err(MokshaMintError::InvoiceNotPaidYet);
        // }

        self.db.remove_pending_invoice(invoice_hash)?;
        self.create_blinded_signatures(outputs)
    }

    fn has_duplicate_pubkeys(outputs: &[BlindedMessage]) -> bool {
        let mut uniq = HashSet::new();
        !outputs.iter().all(move |x| uniq.insert(x.b_))
    }

    pub async fn split(
        &self,
        amount: Option<u64>,
        proofs: &Proofs,
        blinded_messages: &[BlindedMessage],
    ) -> Result<PostSplitResponse, MokshaMintError> {
        self.check_used_proofs(proofs)?;

        if Self::has_duplicate_pubkeys(blinded_messages) {
            return Err(MokshaMintError::SplitHasDuplicatePromises);
        }

        let sum_proofs = proofs.total_amount();

        match amount {
            Some(amount) => {
                if amount > sum_proofs {
                    return Err(MokshaMintError::SplitAmountTooHigh);
                }
                let sum_first = split_amount(sum_proofs - amount).len();
                let first_slice = blinded_messages[0..sum_first].to_vec();
                let first_sigs = self.create_blinded_signatures(&first_slice)?;
                let second_slice = blinded_messages[sum_first..blinded_messages.len()].to_vec();
                let second_sigs = self.create_blinded_signatures(&second_slice)?;

                let amount_first = first_sigs.total_amount();
                let amount_second = second_sigs.total_amount();

                if sum_proofs != (amount_first + amount_second) {
                    return Err(MokshaMintError::SplitAmountMismatch(format!(
                        "Split amount mismatch: {sum_proofs} != {amount_first} + {amount_second}"
                    )));
                }

                self.db.add_used_proofs(proofs)?;
                Ok(PostSplitResponse::with_fst_and_snd(first_sigs, second_sigs))
            }
            None => {
                let promises = self.create_blinded_signatures(blinded_messages)?;
                let amount_promises = promises.total_amount();
                if sum_proofs != amount_promises {
                    return Err(MokshaMintError::SplitAmountMismatch(format!(
                        "Split amount mismatch: {sum_proofs} != {amount_promises}"
                    )));
                }

                self.db.add_used_proofs(proofs)?;
                Ok(PostSplitResponse::with_promises(promises))
            }
        }
    }

    pub async fn melt(
        &self,
        payment_request: String,
        proofs: &Proofs,
        blinded_messages: &[BlindedMessage],
    ) -> Result<(bool, String, Vec<BlindedSignature>), MokshaMintError> {
        let invoice = self
            .lightning
            .decode_invoice(payment_request.clone())
            .await?;

        let proofs_amount = proofs.total_amount();

        // TODO verify proofs

        self.check_used_proofs(proofs)?;

        // TODO check for fees
        let amount_msat = invoice
            .amount_milli_satoshis()
            .expect("Invoice amount is missing");

        if amount_msat < (proofs_amount / 1000) {
            return Err(MokshaMintError::InvoiceAmountTooLow(format!(
                "Invoice amount is too low: {amount_msat}",
            )));
        }

        self.db.add_used_proofs(proofs)?;
        // TODO check invoice

        let result = self.lightning.pay_invoice(payment_request).await?;

        let _remaining_amount = (amount_msat - (proofs_amount / 1000)) * 1000;

        // FIXME check if output amount matches remaining_amount
        let output = self.create_blinded_signatures(blinded_messages)?;

        Ok((true, result.payment_hash, output))
    }

    pub fn check_used_proofs(&self, proofs: &Proofs) -> Result<(), MokshaMintError> {
        let used_proofs = self.db.get_used_proofs()?.proofs();
        for used_proof in used_proofs {
            if proofs.proofs().contains(&used_proof) {
                return Err(MokshaMintError::ProofAlreadyUsed(format!("{used_proof:?}")));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::lightning::error::LightningError;
    use crate::lightning::{LightningType, MockLightning};
    use crate::model::{Invoice, PayInvoiceResult};
    use crate::{database::MockDatabase, error::MokshaMintError, Mint};
    use moksha_core::dhke;
    use moksha_core::model::{BlindedMessage, TokenV3, TotalAmount};
    use moksha_core::model::{PostSplitRequest, Proofs};
    use std::str::FromStr;
    use std::sync::Arc;

    #[test]
    fn test_fee_reserve() -> anyhow::Result<()> {
        let mint = create_mint_from_mocks(None, None);
        let fee = mint.fee_reserve(10000);
        assert_eq!(4000, fee);
        Ok(())
    }

    #[tokio::test]
    async fn test_create_blindsignatures() -> anyhow::Result<()> {
        let mint = create_mint_from_mocks(None, None);

        let blinded_messages = vec![BlindedMessage {
            amount: 8,
            b_: dhke::public_key_from_hex(
                "02634a2c2b34bec9e8a4aba4361f6bf202d7fa2365379b0840afe249a7a9d71239",
            ),
        }];

        let result = mint.create_blinded_signatures(&blinded_messages)?;

        assert_eq!(1, result.len());
        assert_eq!(8, result[0].amount);
        assert_eq!(
            dhke::public_key_from_hex(
                "037074c4f53e326ee14ed67125f387d160e0e729351471b69ad41f7d5d21071e15"
            ),
            result[0].c_
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_mint_empty() -> anyhow::Result<()> {
        let mut lightning = MockLightning::new();
        lightning.expect_is_invoice_paid().returning(|_| Ok(true));
        let mint = create_mint_from_mocks(Some(create_mock_mint()), Some(lightning));

        let outputs = vec![];
        let result = mint.mint_tokens("somehash".to_string(), &outputs).await?;
        assert!(result.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_mint_valid() -> anyhow::Result<()> {
        let mut lightning = MockLightning::new();
        lightning.expect_is_invoice_paid().returning(|_| Ok(true));
        let mint = create_mint_from_mocks(Some(create_mock_mint()), Some(lightning));

        let outputs = create_blinded_msgs_from_fixture("blinded_messages_40.json".to_string())?;
        let result = mint.mint_tokens("somehash".to_string(), &outputs).await?;
        assert_eq!(40, result.total_amount());
        Ok(())
    }

    #[tokio::test]
    async fn test_split_zero() -> anyhow::Result<()> {
        let blinded_messages = vec![];
        let mint = create_mint_from_mocks(Some(create_mock_db_get_used_proofs()), None);

        let proofs = Proofs::empty();
        let result = mint.split(Some(0), &proofs, &blinded_messages).await?;

        assert!(result.promises.is_none());
        assert_eq!(result.fst.unwrap().len(), 0);
        assert_eq!(result.snd.unwrap().len(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn test_split_64_in_20() -> anyhow::Result<()> {
        let mint = create_mint_from_mocks(Some(create_mock_db_get_used_proofs()), None);
        let request = create_request_from_fixture("post_split_request_64_20.json".to_string())?;

        let result = mint
            .split(Some(20), &request.proofs, &request.outputs)
            .await?;

        assert!(result.promises.is_none());
        assert_eq!(result.fst.unwrap().total_amount(), 44);
        assert_eq!(result.snd.unwrap().total_amount(), 20);
        Ok(())
    }

    #[tokio::test]
    async fn test_split_64_in_20_no_amount() -> anyhow::Result<()> {
        let mint = create_mint_from_mocks(Some(create_mock_db_get_used_proofs()), None);
        let request =
            create_request_from_fixture("post_split_request_64_20_no_amount.json".to_string())?;

        let result = mint.split(None, &request.proofs, &request.outputs).await?;

        assert_eq!(result.promises.unwrap().total_amount(), 64);
        assert!(result.fst.is_none());
        assert!(result.snd.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn test_split_64_in_64() -> anyhow::Result<()> {
        let mint = create_mint_from_mocks(Some(create_mock_db_get_used_proofs()), None);
        let request = create_request_from_fixture("post_split_request_64_20.json".to_string())?;

        let result = mint
            .split(Some(64), &request.proofs, &request.outputs)
            .await?;

        assert!(result.promises.is_none());
        assert_eq!(result.fst.unwrap().total_amount(), 0);
        assert_eq!(result.snd.unwrap().total_amount(), 64);
        Ok(())
    }

    #[tokio::test]
    async fn test_split_amount_is_too_high() -> anyhow::Result<()> {
        let mint = create_mint_from_mocks(Some(create_mock_db_get_used_proofs()), None);
        let request = create_request_from_fixture("post_split_request_64_20.json".to_string())?;

        let result = mint
            .split(Some(65), &request.proofs, &request.outputs)
            .await;
        assert!(result.is_err());
        let _err = result.unwrap_err();
        assert!(matches!(MokshaMintError::SplitAmountTooHigh, _err));
        Ok(())
    }

    #[tokio::test]
    async fn test_split_duplicate_key() -> anyhow::Result<()> {
        let mint = create_mint_from_mocks(Some(create_mock_db_get_used_proofs()), None);
        let request =
            create_request_from_fixture("post_split_request_duplicate_key.json".to_string())?;

        let result = mint
            .split(Some(20), &request.proofs, &request.outputs)
            .await;
        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    /// melt 20 sats with 60 tokens and receive 40 tokens as change
    async fn test_melt_overpay() -> anyhow::Result<()> {
        use lightning_invoice::Bolt11Invoice as LNInvoice;

        let mut lightning = MockLightning::new();

        lightning.expect_decode_invoice().returning(|_| {
            Ok(
                // 20 sat
                LNInvoice::from_str("lnbc200n1pj9eanxsp5agdl4rd0twdljpcgmg67dwj9mseu5m4lwfhslkws4uh4m5f5pcrqpp5lvspx676rykr64l02s97wjztcxe355qck0naydrsvvkqw42cc35sdq2f38xy6t5wvxqzjccqpjrzjq027t9tsc6jn5ve2k6gnn689unn8h239juuf9s3ce09aty6ed73t5z7nqsqqsygqqyqqqqqqqqqqqqgq9q9qyysgqs5msn4j9v53fq000zhw0gulkcx2dlnfdt953v2ur7z765jj3m0fx6cppkpjwntq5nsqm273u4eevva508pvepg8mh27sqcd29sfjr4cq255a40").expect("invalid invoice")
            )
        });
        lightning.expect_pay_invoice().returning(|_| {
            Ok(PayInvoiceResult {
                payment_hash: "hash".to_string(),
            })
            .map_err(|_err: LightningError| MokshaMintError::InvoiceNotFound("".to_string()))
        });

        let mint = Mint::new(
            "TEST_PRIVATE_KEY".to_string(),
            "0/0/0/0".to_string(),
            Arc::new(lightning),
            LightningType::Lnbits(Default::default()),
            Arc::new(create_mock_db_get_used_proofs()),
            Default::default(),
            Default::default(),
        );

        let tokens = create_token_from_fixture("token_60.cashu".to_string())?;
        let invoice = "some invoice".to_string();
        let change = create_blinded_msgs_from_fixture("blinded_messages_40.json".to_string())?;

        let (paid, _payment_hash, change) = mint.melt(invoice, &tokens.proofs(), &change).await?;

        assert!(paid);
        assert!(change.total_amount() == 40);
        Ok(())
    }

    // FIXME refactor helper functions
    fn create_token_from_fixture(fixture: String) -> Result<TokenV3, anyhow::Error> {
        let base_dir = std::env::var("CARGO_MANIFEST_DIR")?;
        let raw_token = std::fs::read_to_string(format!("{base_dir}/src/fixtures/{fixture}"))?;
        Ok(raw_token.trim().to_string().try_into()?)
    }

    fn create_request_from_fixture(fixture: String) -> Result<PostSplitRequest, anyhow::Error> {
        let base_dir = std::env::var("CARGO_MANIFEST_DIR")?;
        let raw_token = std::fs::read_to_string(format!("{base_dir}/src/fixtures/{fixture}"))?;
        Ok(serde_json::from_str::<PostSplitRequest>(&raw_token)?)
    }

    fn create_blinded_msgs_from_fixture(
        fixture: String,
    ) -> Result<Vec<BlindedMessage>, anyhow::Error> {
        let base_dir = std::env::var("CARGO_MANIFEST_DIR")?;
        let raw_token = std::fs::read_to_string(format!("{base_dir}/src/fixtures/{fixture}"))?;
        Ok(serde_json::from_str::<Vec<BlindedMessage>>(&raw_token)?)
    }

    fn create_mint_from_mocks(
        mock_db: Option<MockDatabase>,
        mock_ln: Option<MockLightning>,
    ) -> Mint {
        let db = match mock_db {
            Some(db) => Arc::new(db),
            None => Arc::new(MockDatabase::new()),
        };

        let lightning = match mock_ln {
            Some(ln) => Arc::new(ln),
            None => Arc::new(MockLightning::new()),
        };

        //let lightning = Arc::new(MockLightning::new());
        Mint::new(
            "TEST_PRIVATE_KEY".to_string(),
            "0/0/0/0".to_string(),
            lightning,
            LightningType::Lnbits(Default::default()),
            db,
            Default::default(),
            Default::default(),
        )
    }

    fn create_mock_db_get_used_proofs() -> MockDatabase {
        let mut mock_db = MockDatabase::new();
        mock_db
            .expect_get_used_proofs()
            .returning(|| Ok(Proofs::empty()));
        mock_db.expect_add_used_proofs().returning(|_| Ok(()));
        mock_db
    }

    fn create_mock_mint() -> MockDatabase {
        //use lightning_invoice::Invoice as LNInvoice;
        let mut mock_db = MockDatabase::new();
        //let invoice = LNInvoice::from_str("lnbcrt1u1pjgamjepp5cr2dzhcuy9tjwl7u45kxa9h02khvsd2a7f2x9yjxgst8trduld4sdqqcqzzsxqyz5vqsp5kaclwkq79ylef295qj7x6c9kvhaq6272ge4tgz7stlzv46csrzks9qyyssq9szxlvhh0uen2jmh07hp242nj5529wje3x5e434kepjzeqaq5hnsje8rzrl97s0j8cxxt3kgz5gfswrrchr45u8fq3twz2jjc029klqpd6jmgv").expect("invalid invoice");
        let invoice = Invoice{
            amount: 100,
            payment_request: "lnbcrt1u1pjgamjepp5cr2dzhcuy9tjwl7u45kxa9h02khvsd2a7f2x9yjxgst8trduld4sdqqcqzzsxqyz5vqsp5kaclwkq79ylef295qj7x6c9kvhaq6272ge4tgz7stlzv46csrzks9qyyssq9szxlvhh0uen2jmh07hp242nj5529wje3x5e434kepjzeqaq5hnsje8rzrl97s0j8cxxt3kgz5gfswrrchr45u8fq3twz2jjc029klqpd6jmgv".to_string(),            
        };
        mock_db
            .expect_get_used_proofs()
            .returning(|| Ok(Proofs::empty()));
        mock_db
            .expect_remove_pending_invoice()
            .returning(|_| Ok(()));
        mock_db
            .expect_get_pending_invoice()
            .returning(move |_| Ok(invoice.clone()));
        mock_db.expect_add_used_proofs().returning(|_| Ok(()));
        mock_db
    }
}
