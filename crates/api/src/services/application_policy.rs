//! A program URL or a crawl approval never grants authority to submit for a user.
use super::application_destination::{self, Destination};
use super::application_workflow::{self, Workflow};
use chrono::Utc;
use mazimi_core::models::Program;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ApplicationPolicy {
    pub resources: super::application_resources::Resources,
    pub notice_destination: Option<Destination>,
    pub mode: &'static str,
    pub can_prepare: bool,
    pub can_autofill: bool,
    pub can_submit: bool,
    pub reason: &'static str,
    pub official_origin: Option<String>,
    pub destination: Option<Destination>,
    pub workflow: Workflow,
}

pub fn with_resources(
    program: &Program,
    resources: super::application_resources::Resources,
) -> ApplicationPolicy {
    // The worker's selected links; the official URL until it has resolved.
    let (link_application, link_notice) =
        application_destination::fallback(program.official_url.as_deref(), &resources.link);
    let mut workflow = application_workflow::classify(program.application_method.as_deref());
    if resources.application.is_some() && workflow.channels.iter().all(|c| c.id == "unknown") {
        workflow.channels = application_workflow::classify(Some("온라인신청")).channels;
        workflow.needs_confirmation = true;
    }
    let online = workflow.accepts_online();
    let notice_destination = resources
        .notice
        .as_ref()
        .and_then(|r| application_destination::resolve(Some(&r.url)))
        .or(link_notice);
    let destination = resources
        .application
        .as_ref()
        .and_then(application_destination::verified)
        .or(link_application)
        .filter(|_| online)
        .or_else(|| notice_destination.clone());
    let has_method = workflow.channels.iter().any(|c| c.id != "unknown");
    let origin = destination.as_ref().map(|link| link.origin.clone());
    let current = program.is_currently_available(Utc::now());
    ApplicationPolicy {
        resources,
        notice_destination,
        mode: if current && (origin.is_some() || has_method) {
            "assisted"
        } else {
            "unavailable"
        },
        can_prepare: current && (origin.is_some() || has_method),
        // No provider has supplied a contracted submission API yet. This must
        // not be inferred from raw_payload, an LLM output or a crawl permission.
        can_autofill: false,
        can_submit: false,
        reason: if !current {
            "현재 신청 가능한 기간인지 공식 신청처에서 확인해 주세요."
        } else if origin.is_none() && !has_method {
            "공식 신청 주소를 확인하고 있어요."
        } else {
            "정보와 서류 준비를 도와드려요. 본인인증과 최종 제출은 공식 신청처에서 진행해 주세요."
        },
        official_origin: origin,
        destination,
        workflow,
    }
}

pub fn for_channel<'a>(policy: &'a ApplicationPolicy, channel: &str) -> Option<&'a Destination> {
    if matches!(channel, "online" | "unknown") {
        policy.destination.as_ref()
    } else {
        policy.notice_destination.as_ref()
    }
}
