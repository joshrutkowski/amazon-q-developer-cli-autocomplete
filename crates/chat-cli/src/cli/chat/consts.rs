use super::token_counter::TokenCounter;

// These limits are the internal undocumented values from the service for each item

pub const MAX_CURRENT_WORKING_DIRECTORY_LEN: usize = 256;

/// Limit to send the number of messages as part of chat.
pub const MAX_CONVERSATION_STATE_HISTORY_LEN: usize = 250;

/// Actual service limit is 800_000
pub const MAX_TOOL_RESPONSE_SIZE: usize = 600_000;

/// Actual service limit is 600_000
pub const MAX_USER_MESSAGE_SIZE: usize = 600_000;

/// In tokens
pub const CONTEXT_WINDOW_SIZE: usize = 200_000;

pub const CONTEXT_FILES_MAX_SIZE: usize = 150_000;

pub const MAX_CHARS: usize = TokenCounter::token_to_chars(CONTEXT_WINDOW_SIZE); // Character-based warning threshold

pub const DUMMY_TOOL_NAME: &str = "dummy";

pub const MAX_NUMBER_OF_IMAGES_PER_REQUEST: usize = 10;

// Maximum allowed raw image size (approx. 6.66MB), ensuring the base64-encoded size stays within 10MB
pub const MAX_IMAGE_SIZE: usize = (10 * 1024 * 1024) * 2 / 3; // = 6_990_720 bytes