//! Module that encapsulates everything needed to create a new chatroom task by the main broker task
//!

use std::collections::{HashMap, HashSet};
use uuid::Uuid;
use async_std::net;
use async_std::channel::{bounded, unbounded};
use async_std::task;
use futures::{select, Stream, StreamExt, FutureExt};
use tokio::sync::broadcast;

use rusty_chat::prelude::*;
use crate::server_error::prelude::*;
