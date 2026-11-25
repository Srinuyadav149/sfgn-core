use std::collections::{HashSet, VecDeque};

use crate::node::{GraphNode, RevealLevel};

/// Working-memory scratchpad holding the bounded window of active SFGN nodes,
/// visited targets, latest model-reported task progress, and the active error-recovery banner.
#[derive(Debug, Clone)]
pub struct Scratchpad {
    max_active_nodes: usize,
    active_nodes: VecDeque<GraphNode>,
    visited_targets: HashSet<String>,
    failed_signatures: HashSet<String>,
    last_error_banner: Option<String>,
    task_progress: Option<String>,
}

impl Default for Scratchpad {
    fn default() -> Self {
        Self::new(6)
    }
}

impl Scratchpad {
    pub fn new(max_active_nodes: usize) -> Self {
        Self {
            max_active_nodes: max_active_nodes.max(1),
            active_nodes: VecDeque::with_capacity(max_active_nodes.max(1)),
            visited_targets: HashSet::new(),
            failed_signatures: HashSet::new(),
            last_error_banner: None,
            task_progress: None,
        }
    }

    /// Pushes or refreshes a [`GraphNode`] in the active context window, evicts superseded
    /// Level 1 parent outlines when descending into a Level 2 leaf, and clears any error banner.
    pub fn push_node(&mut self, node: GraphNode) {
        self.visited_targets.insert(node.address.clone());

        if node.level == RevealLevel::Level2Leaf {
            if let Some((parent_file, _)) = node.address.split_once("::") {
                self.active_nodes.retain(|existing| {
                    !(existing.address == parent_file
                        && existing.level == RevealLevel::Level1Outline)
                });
            }
            self.evict_tool_branch(&node.address);
        }

        if let Some(pos) = self
            .active_nodes
            .iter()
            .position(|existing| existing.address == node.address)
        {
            self.active_nodes.remove(pos);
        }
        if self.active_nodes.len() >= self.max_active_nodes {
            self.active_nodes.pop_front();
        }
        self.active_nodes.push_back(node);
        self.last_error_banner = None;
    }

    /// Evicts a superseded Level 1 tool branch (`tools/memory/`) once a leaf (`tools/memory/record_memory`) is unlocked.
    pub fn evict_tool_branch(&mut self, leaf_address: &str) {
        if leaf_address.starts_with("tools/") {
            if let Some((branch_prefix, _)) = leaf_address.rsplit_once('/') {
                let branch_addr = format!("{}/", branch_prefix);
                self.active_nodes.retain(|existing| {
                    !(existing.address == branch_addr
                        && existing.level == RevealLevel::Level1Outline)
                });
            }
        }
    }

    pub fn set_task_progress(&mut self, progress: impl Into<String>) {
        let cleaned = progress.into().trim().to_owned();
        if !cleaned.is_empty() {
            self.task_progress = Some(cleaned);
        }
    }

    pub fn task_progress(&self) -> Option<&str> {
        self.task_progress.as_deref()
    }

    pub fn mark_target_visited(&mut self, target: impl Into<String>) {
        self.visited_targets.insert(target.into());
    }

    pub fn has_visited_target(&self, target: &str) -> bool {
        self.visited_targets.contains(target)
    }

    pub fn is_node_active(&self, target: &str) -> bool {
        self.active_nodes.iter().any(|n| n.address == target)
    }

    /// Records a failed action signature and sets the recovery banner shown on the next turn.
    pub fn record_failure(&mut self, call_signature: String, banner: String) {
        self.failed_signatures.insert(call_signature);
        self.last_error_banner = Some(banner);
    }

    pub fn has_failed_before(&self, call_signature: &str) -> bool {
        self.failed_signatures.contains(call_signature)
    }

    pub fn clear_error_banner(&mut self) {
        self.last_error_banner = None;
    }

    pub fn last_error_banner(&self) -> Option<&str> {
        self.last_error_banner.as_deref()
    }

    pub fn active_nodes(&self) -> impl Iterator<Item = &GraphNode> {
        self.active_nodes.iter()
    }
}
