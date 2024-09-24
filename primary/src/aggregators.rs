// Copyright(C) Facebook, Inc. and its affiliates.
use crate::error::{DagError, DagResult};
use crate::messages::{Certificate, NoVoteCert, NoVoteMsg, Timeout, TimeoutCert, Vote};
use blsttc::{PublicKeyShareG2, SignatureShareG1};
use config::{Clan, Committee, Stake};
use crypto::{aggregate_sign, PublicKey, Signature};
use log::info;
use std::collections::HashSet;
use std::sync::Arc;

/// Aggregates votes for a particular header into a certificate.
pub struct VotesAggregator {
    committee_weight: Stake,
    clan_weight: Stake,
    votes: Vec<(PublicKeyShareG2, SignatureShareG1)>,
    used: HashSet<PublicKey>,
    agg_sign: SignatureShareG1,
    pk_bit_vec: Vec<u128>,
    sorted_keys: Arc<Vec<PublicKeyShareG2>>,
}

impl VotesAggregator {
    pub fn new(sorted_keys: Arc<Vec<PublicKeyShareG2>>, total_nodes: usize) -> Self {
        Self {
            committee_weight: 0,
            clan_weight: 0,
            votes: Vec::new(),
            used: HashSet::new(),
            agg_sign: SignatureShareG1::default(),
            pk_bit_vec: vec![u128::MAX; (total_nodes + 127) / 128],
            sorted_keys,
        }
    }

    pub fn append(
        &mut self,
        vote: Vote,
        committee: &Committee,
        clan: &Clan,
    ) -> DagResult<Option<Certificate>> {
        let author = vote.author;
        let author_bls = committee.get_bls_public_g2(&author);

        // Ensure it is the first time this authority votes.
        ensure!(self.used.insert(author), DagError::AuthorityReuse(author));

        self.votes.push((author_bls, vote.signature));
        self.committee_weight += committee.stake(&author);

        if clan.is_member(&author) {
            self.clan_weight += clan.stake(&author);
        }

        let id = self.sorted_keys.binary_search(&author_bls).unwrap();
        let chunk = id / 128;
        let bit = id % 128;
        //adding it to bitvec
        self.pk_bit_vec[chunk] &= !(1 << bit);

        if self.votes.len() == 1 {
            self.agg_sign = vote.signature;
        } else if self.votes.len() >= 2 {
            let new_agg_sign = aggregate_sign(&self.agg_sign, &vote.signature);
            self.agg_sign = new_agg_sign;
        }

        if self.committee_weight >= committee.quorum_threshold()
            && self.clan_weight >= clan.validity_threshold()
        {
            self.committee_weight = 0; // Ensures quorum is only reached once.

            return Ok(Some(Certificate {
                header_id: vote.id,
                round: vote.round,
                origin: vote.origin,
                votes: (self.pk_bit_vec.clone(), self.agg_sign),
            }));
        }
        Ok(None)
    }
}

/// Aggregate certificates and check if we reach a quorum.
pub struct CertificatesAggregator {
    weight: Stake,
    certificates: Vec<Certificate>,
    used: HashSet<PublicKey>,
}

impl CertificatesAggregator {
    pub fn new() -> Self {
        Self {
            weight: 0,
            certificates: Vec::new(),
            used: HashSet::new(),
        }
    }

    pub fn append(
        &mut self,
        certificate: Certificate,
        committee: &Committee,
        leaders_per_round: usize,
    ) -> DagResult<Option<Vec<Certificate>>> {
        let origin = certificate.origin();

        // Ensure it is the first time this authority votes.
        if !self.used.insert(origin) {
            return Ok(None);
        }

        let round = certificate.round;

        self.certificates.push(certificate.clone());
        self.weight += committee.stake(&origin);

        let leaders = committee.leader_list(leaders_per_round, round as usize);
        for leader in leaders.iter() {
            if !self.used.contains(leader) {
                return Ok(None);
            }
        }

        info!("Got all leader for round {}", certificate.round());

        if self.weight >= committee.quorum_threshold() {
            // self.weight = 0; // Ensures quorum is only reached once.
            return Ok(Some(self.certificates.drain(..).collect()));
        }
        Ok(None)
    }
}

/// Aggregates timeouts for a particular round into an action or trigger.
pub struct TimeoutAggregator {
    weight: Stake,
    timeouts: Vec<(PublicKey, Signature)>,
    used: HashSet<PublicKey>,
}

impl TimeoutAggregator {
    pub fn new() -> Self {
        Self {
            weight: 0,
            timeouts: Vec::new(),
            used: HashSet::new(),
        }
    }

    pub fn append(
        &mut self,
        timeout: Timeout,
        committee: &Committee,
    ) -> DagResult<Option<TimeoutCert>> {
        let author = timeout.author;

        // Ensure it is the first time this authority sends a timeout.
        ensure!(self.used.insert(author), DagError::AuthorityReuse(author));

        self.timeouts.push((author, timeout.signature));
        self.weight += committee.stake(&author);
        if self.weight >= committee.quorum_threshold() {
            // Once quorum is reached, you might want to reset for the next round or trigger an action.
            return Ok(Some(TimeoutCert {
                round: timeout.round.clone(),
                timeouts: self.timeouts.clone(),
            })); // Return the authorities that contributed to this quorum.
        }
        Ok(None)
    }
}

/// Aggregates no-vote messages for a particular round into a certification.
pub struct NoVoteAggregator {
    weight: Stake,
    no_votes: Vec<(PublicKey, Signature)>,
    used: HashSet<PublicKey>,
}

impl NoVoteAggregator {
    pub fn new() -> Self {
        Self {
            weight: 0,
            no_votes: Vec::new(),
            used: HashSet::new(),
        }
    }

    pub fn append(
        &mut self,
        no_vote_msg: NoVoteMsg,
        committee: &Committee,
    ) -> DagResult<Option<NoVoteCert>> {
        let author = no_vote_msg.author;

        // Ensure it is the first time this authority sends a no-vote message.
        ensure!(self.used.insert(author), DagError::AuthorityReuse(author));

        self.no_votes.push((author, no_vote_msg.signature));
        self.weight += committee.stake(&author);
        if self.weight >= committee.quorum_threshold() {
            // Once quorum is reached, you might reset for the next round or use the certification as needed.
            return Ok(Some(NoVoteCert {
                round: no_vote_msg.round.clone(),
                no_votes: self.no_votes.clone(),
            })); // Return the certification that aggregates the no-votes reaching quorum.
        }
        Ok(None)
    }
}
