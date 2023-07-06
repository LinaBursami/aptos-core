// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::dag::{
    storage::DAGStorage,
    types::{CertifiedNode, NodeCertificate, NodeMetadata},
};
use anyhow::{anyhow, ensure};
use aptos_consensus_types::common::{Author, Round};
use aptos_logger::error;
use aptos_types::{epoch_state::EpochState, validator_verifier::ValidatorVerifier};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

#[derive(Clone)]
enum NodeStatus {
    Unordered(Arc<CertifiedNode>),
    Ordered(Arc<CertifiedNode>),
    Committed(Arc<CertifiedNode>),
}

impl NodeStatus {
    fn as_node(&self) -> &Arc<CertifiedNode> {
        match self {
            NodeStatus::Unordered(node)
            | NodeStatus::Ordered(node)
            | NodeStatus::Committed(node) => node,
        }
    }
}

/// Data structure that stores the DAG representation, it maintains round based index.
pub struct Dag {
    nodes_by_round: BTreeMap<Round, Vec<Option<NodeStatus>>>,
    /// Map between peer id to vector index
    author_to_index: HashMap<Author, usize>,
    storage: Arc<dyn DAGStorage>,
}

impl Dag {
    pub fn new(epoch_state: Arc<EpochState>, storage: Arc<dyn DAGStorage>) -> Self {
        let epoch = epoch_state.epoch;
        let author_to_index = epoch_state.verifier.address_to_validator_index().clone();
        let num_validators = author_to_index.len();
        let all_nodes = storage.get_certified_nodes().unwrap_or_default();
        let mut expired = vec![];
        let mut nodes_by_round = BTreeMap::new();
        for (digest, certified_node) in all_nodes {
            if certified_node.metadata().epoch() == epoch {
                let arc_node = Arc::new(certified_node);
                let index = *author_to_index
                    .get(arc_node.metadata().author())
                    .expect("Author from certified node should exist");
                let round = arc_node.metadata().round();
                nodes_by_round
                    .entry(round)
                    .or_insert_with(|| vec![None; num_validators])[index] =
                    Some(NodeStatus::Unordered(arc_node));
            } else {
                expired.push(digest);
            }
        }
        if let Err(e) = storage.delete_certified_nodes(expired) {
            error!("Error deleting expired nodes: {:?}", e);
        }
        Self {
            nodes_by_round,
            author_to_index,
            storage,
        }
    }

    pub(crate) fn lowest_round(&self) -> Round {
        *self
            .nodes_by_round
            .first_key_value()
            .map(|(round, _)| round)
            .unwrap_or(&0)
    }

    pub fn highest_round(&self) -> Round {
        *self
            .nodes_by_round
            .last_key_value()
            .map(|(round, _)| round)
            .unwrap_or(&0)
    }

    pub fn add_node(&mut self, node: CertifiedNode) -> anyhow::Result<()> {
        let node = Arc::new(node);
        let author = node.metadata().author();
        let index = *self
            .author_to_index
            .get(author)
            .ok_or_else(|| anyhow!("unknown author"))?;
        let round = node.metadata().round();
        ensure!(round >= self.lowest_round(), "round too low");
        ensure!(round <= self.highest_round() + 1, "round too high");
        for parent in node.parents() {
            ensure!(self.exists(parent.metadata()), "parent not exist");
        }
        let round_ref = self
            .nodes_by_round
            .entry(round)
            .or_insert_with(|| vec![None; self.author_to_index.len()]);
        ensure!(round_ref[index].is_none(), "duplicate node");

        // mutate after all checks pass
        self.storage.save_certified_node(&node)?;
        round_ref[index] = Some(NodeStatus::Unordered(node.clone()));
        Ok(())
    }

    pub fn exists(&self, metadata: &NodeMetadata) -> bool {
        self.get_node_ref(metadata).is_some()
    }

    pub fn all_exists(&self, nodes: &[NodeCertificate]) -> bool {
        nodes
            .iter()
            .all(|certificate| self.exists(certificate.metadata()))
    }

    fn get_node_ref(&self, metadata: &NodeMetadata) -> Option<&NodeStatus> {
        let index = self.author_to_index.get(metadata.author())?;
        let round_ref = self.nodes_by_round.get(&metadata.round())?;
        round_ref[*index].as_ref()
    }

    pub fn get_node(&self, metadata: &NodeMetadata) -> Option<Arc<CertifiedNode>> {
        self.get_node_ref(metadata)
            .map(|node_status| node_status.as_node().clone())
    }

    pub fn get_strong_links_for_round(
        &self,
        round: Round,
        validator_verifier: &ValidatorVerifier,
    ) -> Option<Vec<NodeCertificate>> {
        let all_nodes_in_round = self.nodes_by_round.get(&round)?.iter().flatten();
        if validator_verifier
            .check_voting_power(
                all_nodes_in_round
                    .clone()
                    .map(|node_status| node_status.as_node().metadata().author()),
            )
            .is_ok()
        {
            Some(
                all_nodes_in_round
                    .map(|node_status| node_status.as_node().certificate())
                    .collect(),
            )
        } else {
            None
        }
    }

    pub fn bitmask(&self) -> Vec<Vec<bool>> {
        // TODO: extract local bitvec
        todo!();
    }
}
