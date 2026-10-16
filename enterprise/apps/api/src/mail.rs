//! Outgoing mail: sign-in links, invitations, notices. SMTP in staging and production; in
//! development without SMTP, mail is kept in the dev_outbox table (shown by the portal's
//! development banner) instead of being sent.

use async_trait::async_trait;

#[async_trait]
pub trait Mailer: Send + Sync {
    async fn send(&self, to: &str, subject: &str, body: &str) -> anyhow::Result<()>;
    fn kind(&self) -> &'static str;
}

pub struct Smtp {
    transport: lettre::AsyncSmtpTransport<lettre::Tokio1Executor>,
    from: lettre::message::Mailbox,
}

impl Smtp {
    pub fn new(url: &str, from: &str) -> anyhow::Result<Smtp> {
        let transport = lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::from_url(url)
            .map_err(|e| {
                anyhow::anyhow!("TARO_SMTP_URL: {}", crate::redact::redact(&e.to_string()))
            })?
            .build();
        Ok(Smtp {
            transport,
            from: from.parse()?,
        })
    }
}

#[async_trait]
impl Mailer for Smtp {
    async fn send(&self, to: &str, subject: &str, body: &str) -> anyhow::Result<()> {
        use lettre::AsyncTransport;
        let m = lettre::Message::builder()
            .from(self.from.clone())
            .to(to.parse()?)
            .subject(subject)
            .body(body.to_string())?;
        self.transport.send(m).await?;
        Ok(())
    }
    fn kind(&self) -> &'static str {
        "smtp"
    }
}

pub struct DevOutbox {
    pub db: sqlx::PgPool,
}

#[async_trait]
impl Mailer for DevOutbox {
    async fn send(&self, to: &str, subject: &str, body: &str) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO dev_outbox (id, to_email, subject, body) VALUES ($1, $2, $3, $4)")
            .bind(uuid::Uuid::new_v4())
            .bind(to)
            .bind(subject)
            .bind(body)
            .execute(&self.db)
            .await?;
        // the body holds a sign-in link: never in the log
        tracing::info!(
            subject,
            "mail kept in the development outbox (no SMTP configured)"
        );
        Ok(())
    }
    fn kind(&self) -> &'static str {
        "dev_outbox"
    }
}
