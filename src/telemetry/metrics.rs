#[derive(Clone, Copy, Debug)]
pub enum Operation {
    HttpRequest,
    ReadinessProbe,
    Startup,
    Shutdown,
}

impl Operation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HttpRequest => "http.request",
            Self::ReadinessProbe => "health.readiness",
            Self::Startup => "server.startup",
            Self::Shutdown => "server.shutdown",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Outcome {
    Success,
    Rejected,
    Unavailable,
    Failure,
}

impl Outcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Rejected => "rejected",
            Self::Unavailable => "unavailable",
            Self::Failure => "failure",
        }
    }
}

pub fn record_operation(operation: Operation, outcome: Outcome) {
    tracing::debug!(
        operation = operation.as_str(),
        outcome = outcome.as_str(),
        "operation outcome"
    );
}
