# Copyright(C) Facebook, Inc. and its affiliates.
from datetime import datetime
from glob import glob
from multiprocessing import Pool
from os.path import join
from re import findall, search
from statistics import mean
import csv
from benchmark.utils import Print


class ParseError(Exception):
    pass


class LogParser:
    def __init__(self, clients, primaries, burst, faults=0, consensus_only=False):

        inputs = [primaries]

        if not consensus_only:
            inputs += [clients]

        assert all(isinstance(x, list) for x in inputs)
        assert all(isinstance(x, str) for y in inputs for x in y)
        assert all(x for x in inputs)

        self.consensus_only = consensus_only
        self.burst = burst
        self.faults = faults
        if isinstance(faults, int):
            self.committee_size = len(primaries) + int(faults)
        else:
            self.committee_size = '?'
            self.workers = '?'

        if not consensus_only:
            # Parse the clients logs.
            try:
                with Pool() as p:
                    results = p.map(self._parse_clients, clients)
            except (ValueError, IndexError, AttributeError) as e:
                raise ParseError(f'Failed to parse clients\' logs: {e}')
            self.size, self.rate, self.start, misses, self.sent_samples \
                = zip(*results)
            self.misses = sum(misses)

        # Parse the primaries logs.
        try:
            with Pool() as p:
                results = p.map(self._parse_primaries, primaries)
        except (ValueError, IndexError, AttributeError) as e:
            raise ParseError(f'Failed to parse nodes\' logs: {e}')

        proposals, commits, self.configs, primary_ips, leader_commits, non_leader_commits, self.received_samples, sizes = zip(
            *results)
        self.proposals = self._merge_results([x.items() for x in proposals])
        self.commits = self._merge_results([x.items() for x in commits])
        self.leader_commits = self._merge_results(
            [x.items() for x in leader_commits])
        self.non_leader_commits = self._merge_results(
            [x.items() for x in non_leader_commits])

        self.sizes = {
            k: v for x in sizes for k, v in x.items() if k in self.commits
        }
        # # Parse the workers logs.
        # try:
        #     with Pool() as p:
        #         results = p.map(self._parse_workers, workers)
        # except (ValueError, IndexError, AttributeError) as e:
        #     raise ParseError(f'Failed to parse workers\' logs: {e}')
        # sizes, self.received_samples, workers_ips = zip(*results)

        # # Determine whether the primary and the workers are collocated.
        # self.collocate = set(primary_ips) == set(workers_ips)
        self.collocate = True

        if not self.consensus_only:
            # Check whether clients missed their target rate.
            if self.misses != 0:
                Print.warn(
                    f'Clients missed their target rate {self.misses:,} time(s)'
                )

    def _merge_results(self, input):
        # Keep the earliest timestamp.
        merged = {}
        for x in input:
            for k, v in x:
                if not k in merged or merged[k] > v:
                    merged[k] = v
        return merged

    def _parse_clients(self, log):
        if search(r'Error', log) is not None:
            raise ParseError('Client(s) panicked')

        size = int(search(r'Transactions size: (\d+)', log).group(1))
        rate = int(search(r'Transactions rate: (\d+)', log).group(1))

        tmp = search(r'\[(.*Z) .* Start ', log).group(1)
        start = self._to_posix(tmp)

        misses = len(findall(r'rate too high', log))

        tmp = findall(r'\[(.*Z) .* sample transaction (\d+)', log)
        samples = {int(s): self._to_posix(t) for t, s in tmp}

        return size, rate, start, misses, samples

    def _parse_primaries(self, log):
        if search(r'(?:panicked|Error)', log) is not None:
            raise ParseError('Primary(s) panicked')

        tmp = findall(r'\[(.*Z) .* Created ([^ ]+)\n', log)
        tmp = [(d, self._to_posix(t)) for t, d in tmp]
        proposals = self._merge_results([tmp])

        tmp = findall(r'\[(.*Z) .* Committed ([^ ]+)', log)
        tmp = [(d, self._to_posix(t)) for t, d in tmp]
        commits = self._merge_results([tmp])

        tmp = findall(r'\[(.*Z) .* Committed ([^ ]+) Leader', log)
        tmp = [(d, self._to_posix(t)) for t, d in tmp]
        leader_commits = self._merge_results([tmp])

        tmp = findall(r'\[(.*Z) .* Committed ([^ ]+) NonLeader', log)
        tmp = [(d, self._to_posix(t)) for t, d in tmp]
        non_leader_commits = self._merge_results([tmp])

        if self.consensus_only:
            samples = {}
            tmp = findall(r'Header ([^ ]+) contains (\d+) B', log)
            sizes = {d: int(s) for d, s in tmp}
        else:
            tmp = findall(r'Header ([^ ]+) contains sample tx (\d+)', log)
            samples = {int(s): d for d, s in tmp}

            tmp = findall(r'Header ([^ ]+) contains (\d+) B', log)
            sizes = {d: int(s) for d, s in tmp}

        configs = {
            'header_size': int(
                search(r'Header size .* (\d+)', log).group(1)
            ),
            'max_header_delay': int(
                search(r'Max header delay .* (\d+)', log).group(1)
            ),
            'gc_depth': int(
                search(r'Garbage collection depth .* (\d+)', log).group(1)
            ),
            'sync_retry_delay': int(
                search(r'Sync retry delay .* (\d+)', log).group(1)
            ),
            'sync_retry_nodes': int(
                search(r'Sync retry nodes .* (\d+)', log).group(1)
            ),
            'batch_size': int(
                search(r'Batch size .* (\d+)', log).group(1)
            ),
            'max_batch_delay': int(
                search(r'Max batch delay .* (\d+)', log).group(1)
            ),
            'transaction_size': int(
                search(r'Transaction size .* (\d+)', log).group(1)
            ),
            'leaders_per_round': int(
                search(r'Leaders per round .* (\d+)', log).group(1)
            ),
        }

        ip = search(r'booted on (\d+.\d+.\d+.\d+)', log).group(1)

        return proposals, commits, configs, ip, leader_commits, non_leader_commits, samples, sizes

    # def _parse_workers(self, log):
    #     if search(r'(?:panic|Error)', log) is not None:
    #         raise ParseError('Worker(s) panicked')

    #     tmp = findall(r'Batch ([^ ]+) contains (\d+) B', log)
    #     sizes = {d: int(s) for d, s in tmp}

    #     tmp = findall(r'Batch ([^ ]+) contains sample tx (\d+)', log)
    #     samples = {int(s): d for d, s in tmp}

    #     ip = search(r'booted on (\d+.\d+.\d+.\d+)', log).group(1)

    #     return sizes, samples, ip

    def _throughput(self, start, commits):
        if not commits:
            return 0, 0, 0
        end = max(commits.values())
        duration = end - start
        total_commits = len(commits.keys())
        commits_per_second = total_commits / duration
        return total_commits, commits_per_second, duration

    def _to_posix(self, string):
        x = datetime.fromisoformat(string.replace('Z', '+00:00'))
        return datetime.timestamp(x)

    def _consensus_throughput(self):
        if not self.commits:
            return 0, 0, 0
        start, end = min(self.proposals.values()), max(self.commits.values())
        duration = end - start
        bytes = sum(self.sizes.values())
        bps = bytes / duration
        tps = bps / self.size[0]
        return tps, bps, duration

    def _consensus_only_throughput(self):
        if not self.commits:
            return 0, 0, 0
        start, end = min(self.proposals.values()), max(self.commits.values())
        bytes = sum(self.sizes.values())
        tx_size = self.configs[0]['transaction_size']
        txns = bytes / tx_size
        d = end-start
        return txns/d

    def _consensus_latency(self):
        latency = [c - self.proposals[d] for d, c in self.commits.items()]
        return mean(latency) if latency else 0

    def _consensus_leader_latency(self):
        latency = [c - self.proposals[d]
                   for d, c in self.leader_commits.items()]
        return mean(latency) if latency else 0

    def _consensus_non_leader_latency(self):
        latency = [c - self.proposals[d]
                   for d, c in self.non_leader_commits.items()]
        return mean(latency) if latency else 0

    def _end_to_end_throughput(self):
        if not self.commits:
            return 0, 0, 0
        start, end = min(self.start), max(self.commits.values())
        duration = end - start
        bytes = sum(self.sizes.values())
        bps = bytes / duration
        tps = bps / self.size[0]
        return tps, bps, duration

    def _end_to_end_latency(self):

        latency = []
        for sent, received in zip(self.sent_samples, self.received_samples):
            for tx_id, header_id in received.items():
                if header_id in self.commits:
                    assert tx_id in sent  # We receive txs that we sent.
                    start = sent[tx_id]
                    end = self.commits[header_id]
                    latency += [end-start]
        return mean(latency) if latency else 0

    def result(self):
        first_proposal_time = min(self.proposals.values())
        start, end = min(self.proposals.values()), max(self.commits.values())
        duration = end - start

        header_size = self.configs[0]['header_size']
        max_header_delay = self.configs[0]['max_header_delay']
        gc_depth = self.configs[0]['gc_depth']
        sync_retry_delay = self.configs[0]['sync_retry_delay']
        sync_retry_nodes = self.configs[0]['sync_retry_nodes']
        batch_size = self.configs[0]['batch_size']
        max_batch_delay = self.configs[0]['max_batch_delay']

        consensus_latency = self._consensus_latency() * 1_000
        leader_consensus_latency = self._consensus_leader_latency() * 1_000
        non_leader_consensus_latency = self._consensus_non_leader_latency() * 1_000

        _, blps_first, _ = self._throughput(first_proposal_time, self.commits)

        consensus_tps = 0
        consensus_bps = 0
        end_to_end_bps = 0
        end_to_end_tps = 0
        end_to_end_latency = 0

        if not self.consensus_only:
            consensus_tps, consensus_bps, _ = self._consensus_throughput()
            end_to_end_tps, end_to_end_bps, duration = self._end_to_end_throughput()
            end_to_end_latency = self._end_to_end_latency() * 1_000
        else:
            consensus_tps = self._consensus_only_throughput()

        leaders_per_round = self.configs[0]['leaders_per_round']
        header_size = self.configs[0]['header_size']
        csv_file_path = f'benchmark_{self.committee_size}_{leaders_per_round}.csv'
        write_to_csv(round(leader_consensus_latency), round(non_leader_consensus_latency), round(consensus_tps), round(consensus_bps), round(
            consensus_latency), round(end_to_end_tps), round(end_to_end_bps), round(end_to_end_latency), self.burst, header_size, csv_file_path)

        if self.consensus_only:
            return (
                '\n'
                '-----------------------------------------\n'
                ' SUMMARY:\n'
                '-----------------------------------------\n'
                ' + CONFIG:\n'
                f' Faults: {self.faults} node(s)\n'
                f' Committee size: {self.committee_size} node(s)\n'
                f' Worker(s) per node: 1 worker\n'
                f' Collocate primary and workers: {self.collocate}\n'
                f' Execution time: {round(duration):,} s\n'
                '\n'
                f' Header size: {header_size:,} B\n'
                f' Max header delay: {max_header_delay:,} ms\n'
                f' GC depth: {gc_depth:,} round(s)\n'
                f' Sync retry delay: {sync_retry_delay:,} ms\n'
                f' Sync retry nodes: {sync_retry_nodes:,} node(s)\n'
                f' batch size: {batch_size:,} B\n'
                f' Max batch delay: {max_batch_delay:,} ms\n'
                '\n'
                ' + RESULTS:\n'
                f' Consensus BLPS: {round(blps_first):,} Block/s\n'
                f' Consensus TPS: {round(consensus_tps):,} tx/s\n'
                f' Consensus latency: {round(consensus_latency):,} ms\n'
                f' Consensus leader latency: {round(leader_consensus_latency):,} ms\n'
                f' Consensus non leader latency: {round(non_leader_consensus_latency):,} ms\n'
                '-----------------------------------------\n'
            )
        else:
            return (
                '\n'
                '-----------------------------------------\n'
                ' SUMMARY:\n'
                '-----------------------------------------\n'
                ' + CONFIG:\n'
                f' Faults: {self.faults} node(s)\n'
                f' Committee size: {self.committee_size} node(s)\n'
                f' Worker(s) per node: 1 worker\n'
                f' Collocate primary and workers: {self.collocate}\n'
                f' Input rate: {sum(self.rate):,} tx/s\n'
                f' Transaction size: {self.size[0]:,} B\n'
                f' Execution time: {round(duration):,} s\n'
                '\n'
                f' Header size: {header_size:,} B\n'
                f' Max header delay: {max_header_delay:,} ms\n'
                f' GC depth: {gc_depth:,} round(s)\n'
                f' Sync retry delay: {sync_retry_delay:,} ms\n'
                f' Sync retry nodes: {sync_retry_nodes:,} node(s)\n'
                f' batch size: {batch_size:,} B\n'
                f' Max batch delay: {max_batch_delay:,} ms\n'
                '\n'
                ' + RESULTS:\n'
                f' Consensus TPS: {round(consensus_tps):,} tx/s\n'
                f' Consensus BPS: {round(consensus_bps):,} B/s\n'
                f' Consensus latency: {round(consensus_latency):,} ms\n'
                f' Consensus leader latency: {round(leader_consensus_latency):,} ms\n'
                f' Consensus non leader latency: {round(non_leader_consensus_latency):,} ms\n'
                '\n'
                f' End-to-end TPS: {round(end_to_end_tps):,} tx/s\n'
                f' End-to-end BPS: {round(end_to_end_bps):,} B/s\n'
                f' End-to-end latency: {round(end_to_end_latency):,} ms\n'
                '-----------------------------------------\n'
            )

    def print(self, filename):
        assert isinstance(filename, str)
        with open(filename, 'a') as f:
            f.write(self.result())

    @classmethod
    def process(cls, directory, burst, faults=0, consensus_only=False):
        assert isinstance(directory, str)

        primaries = []
        for filename in sorted(glob(join(directory, 'primary-*.log'))):
            with open(filename, 'r') as f:
                primaries += [f.read()]

        clients = []
        if not consensus_only:
            for filename in sorted(glob(join(directory, 'client-*.log'))):
                with open(filename, 'r') as f:
                    clients += [f.read()]
        # workers = []
        # for filename in sorted(glob(join(directory, 'worker-*.log'))):
        #     with open(filename, 'r') as f:
        #         workers += [f.read()]

        return cls(clients, primaries, burst, faults=faults, consensus_only=consensus_only)


def write_to_csv(con_r0_latency, con_r1_latency, consensus_tps, consensus_bps, consensus_latency, e2e_tps, e2e_bps, e2e_latency, burst, header_size, csv_file_path):
    # Open the CSV file in append mode
    with open(csv_file_path, mode='a', newline='') as csv_file:
        writer = csv.writer(csv_file)
        column_names = ['Consensus R Latency', 'Consensus R-1 Latency', 'Consensus Tps', 'Consensus Bps',
                        'Consensus Latency', 'E2E Tps', 'E2E Bps', 'E2E Latency', 'Burst', 'Header size']
        # If the file is empty, write the header
        if csv_file.tell() == 0:
            writer.writerow(column_names)

        # Write the extracted data to the CSV file
        writer.writerow([con_r0_latency, con_r1_latency, consensus_tps, consensus_bps,
                        consensus_latency, e2e_tps, e2e_bps, e2e_latency, burst, header_size])
