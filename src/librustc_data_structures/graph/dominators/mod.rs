//! Algorithm citation:
//! A Simple, Fast Dominance Algorithm.
//! Keith D. Cooper, Timothy J. Harvey, and Ken Kennedy
//! Rice Computer Science TS-06-33870
//! <https://www.cs.rice.edu/~keith/EMBED/dom.pdf>

use super::super::indexed_vec::{Idx, IndexVec};
use super::iterate::reverse_post_order;
use super::ControlFlowGraph;
use crate::bit_set::BitSet;

use std::fmt;

#[cfg(test)]
mod test;

pub fn dominators<G: ControlFlowGraph>(graph: &G) -> Dominators<G::Node> {
    let start_node = graph.start_node();
    let rpo = reverse_post_order(graph, start_node);
    dominators_given_rpo(graph, &rpo)
}

pub fn dominators_given_rpo<G: ControlFlowGraph>(
    graph: &G,
    rpo: &[G::Node],
) -> Dominators<G::Node> {
    let start_node = graph.start_node();
    assert_eq!(rpo[0], start_node);

    // compute the post order index (rank) for each node
    let mut post_order_rank: IndexVec<G::Node, usize> =
        (0..graph.num_nodes()).map(|_| 0).collect();
    for (index, node) in rpo.iter().rev().cloned().enumerate() {
        post_order_rank[node] = index;
    }

    let mut immediate_dominators: IndexVec<G::Node, Option<G::Node>> =
        (0..graph.num_nodes()).map(|_| None).collect();
    immediate_dominators[start_node] = Some(start_node);

    let mut changed = true;
    while changed {
        changed = false;

        for &node in &rpo[1..] {
            let mut new_idom = None;
            for pred in graph.predecessors(node) {
                if immediate_dominators[pred].is_some() {
                    // (*)
                    // (*) dominators for `pred` have been calculated
                    new_idom = intersect_opt(
                        &post_order_rank,
                        &immediate_dominators,
                        new_idom,
                        Some(pred),
                    );
                }
            }

            if new_idom != immediate_dominators[node] {
                immediate_dominators[node] = new_idom;
                changed = true;
            }
        }
    }

    Dominators {
        post_order_rank,
        immediate_dominators,
    }
}

fn intersect_opt<Node: Idx>(
    post_order_rank: &IndexVec<Node, usize>,
    immediate_dominators: &IndexVec<Node, Option<Node>>,
    node1: Option<Node>,
    node2: Option<Node>,
) -> Option<Node> {
    match (node1, node2) {
        (None, None) => None,
        (Some(n), None) | (None, Some(n)) => Some(n),
        (Some(n1), Some(n2)) => Some(intersect(post_order_rank, immediate_dominators, n1, n2)),
    }
}

fn intersect<Node: Idx>(
    post_order_rank: &IndexVec<Node, usize>,
    immediate_dominators: &IndexVec<Node, Option<Node>>,
    mut node1: Node,
    mut node2: Node,
) -> Node {
    while node1 != node2 {
        while post_order_rank[node1] < post_order_rank[node2] {
            node1 = immediate_dominators[node1].unwrap();
        }

        while post_order_rank[node2] < post_order_rank[node1] {
            node2 = immediate_dominators[node2].unwrap();
        }
    }

    node1
}

#[derive(Clone, Debug)]
pub struct Dominators<N: Idx> {
    post_order_rank: IndexVec<N, usize>,
    immediate_dominators: IndexVec<N, Option<N>>,
}

impl<Node: Idx> Dominators<Node> {
    pub fn is_reachable(&self, node: Node) -> bool {
        self.immediate_dominators[node].is_some()
    }

    pub fn immediate_dominator(&self, node: Node) -> Node {
        assert!(self.is_reachable(node), "node {:?} is not reachable", node);
        self.immediate_dominators[node].unwrap()
    }

    /// Find the children of a node in the dominator tree.
    pub fn immediately_dominates(&self, node: Node) -> impl Iterator<Item=Node> + '_ {
        self.immediate_dominators.iter().enumerate()
            // Index 0 is the root of the dominator tree. It contains a dummy value (itself), which
            // we must skip or we will end up with infinite loops.
            .skip(1).filter(move |(_, d)| d.is_some() && d.unwrap() == node)
            .map(|(i, _)| Node::new(i))
    }

    pub fn dominators(&self, node: Node) -> Iter<'_, Node> {
        assert!(self.is_reachable(node), "node {:?} is not reachable", node);
        Iter {
            dominators: self,
            node: Some(node),
        }
    }

    pub fn is_dominated_by(&self, node: Node, dom: Node) -> bool {
        // FIXME -- could be optimized by using post-order-rank
        self.dominators(node).any(|n| n == dom)
    }

    #[cfg(test)]
    fn all_immediate_dominators(&self) -> &IndexVec<Node, Option<Node>> {
        &self.immediate_dominators
    }
}

pub struct Iter<'dom, Node: Idx> {
    dominators: &'dom Dominators<Node>,
    node: Option<Node>,
}

impl<'dom, Node: Idx> Iterator for Iter<'dom, Node> {
    type Item = Node;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(node) = self.node {
            let dom = self.dominators.immediate_dominator(node);
            if dom == node {
                self.node = None; // reached the root
            } else {
                self.node = Some(dom);
            }
            return Some(node);
        } else {
            return None;
        }
    }
}

pub struct DominatorFrontiers<G: ControlFlowGraph> {
    dfs: IndexVec<G::Node, BitSet<G::Node>>,
}

impl<G: ControlFlowGraph> DominatorFrontiers<G> {
    pub fn new(graph: &G, doms: &Dominators<G::Node>) -> Self {
        let num_nodes = graph.num_nodes();
        let mut dfs = IndexVec::from_elem_n(BitSet::new_empty(num_nodes), num_nodes);

        for b in (0..num_nodes).map(|i| G::Node::new(i)) {
            if graph.predecessors(b).take(2).count() > 1 {
                // Iterator isn't clonable, so we have to make a new one.
                let preds = graph.predecessors(b);
                for p in preds {
                    let mut runner = p; // Not strictly necessary, but for parity with the paper.
                    while runner != doms.immediate_dominator(b) {
                        dfs[runner].insert(b);
                        runner = doms.immediate_dominator(runner);
                    }
                }
            }
        }
       Self { dfs }
    }

    pub fn frontier(&self, n: G::Node) -> &BitSet<G::Node> {
        &self.dfs[n]
    }
}

pub struct DominatorTree<N: Idx> {
    root: N,
    children: IndexVec<N, Vec<N>>,
}

impl<Node: Idx> DominatorTree<Node> {
    pub fn children(&self, node: Node) -> &[Node] {
        &self.children[node]
    }
}

impl<Node: Idx> fmt::Debug for DominatorTree<Node> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(
            &DominatorTreeNode {
                tree: self,
                node: self.root,
            },
            fmt,
        )
    }
}

struct DominatorTreeNode<'tree, Node: Idx> {
    tree: &'tree DominatorTree<Node>,
    node: Node,
}

impl<'tree, Node: Idx> fmt::Debug for DominatorTreeNode<'tree, Node> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        let subtrees: Vec<_> = self.tree
            .children(self.node)
            .iter()
            .map(|&child| DominatorTreeNode {
                tree: self.tree,
                node: child,
            })
            .collect();
        fmt.debug_tuple("")
            .field(&self.node)
            .field(&subtrees)
            .finish()
    }
}
