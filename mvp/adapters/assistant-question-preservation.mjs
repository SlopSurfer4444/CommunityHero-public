// Final assessment declarations, never inherited item tags, drive this guard.
// It does not classify prose, invent a reply, or authorize a company action.
// A question alone can legitimately close after an answer; an unresolved
// indispensable fact/context blocker cannot coherently close that question.
export function preserveUnresolvedSubstantiveQuestions(candidate) {
  if (!Array.isArray(candidate?.assessments) || !Array.isArray(candidate?.proposals)) return candidate;
  const held = new Map(candidate.assessments.filter(row =>
    row.tags?.some(tag => tag === 'question' || tag === 'complaint')
    && (row.outcome === 'close' && row.tags.some(tag => tag === 'needs_fact' || tag === 'missing_context')
      || row.outcome === 'reply' && row.tags.includes('missing_context')))
    .map(row => [row.itemId, row]));
  if (!held.size) return candidate;
  return {...candidate,
    proposals: candidate.proposals.filter(proposal => !held.has(proposal.itemId)),
    assessments: candidate.assessments.map(row => {
      if (!held.has(row.itemId)) return row;
      const prefix='Содержательный вопрос или жалоба требуют получения заявленного необходимого контекста или решения. ';
      return {...row, outcome:'needs_attention',
        reason: prefix.length + row.reason.length <= 2000 ? prefix + row.reason : row.reason};
    })
  };
}
