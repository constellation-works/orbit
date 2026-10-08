// Realistic receipt shared by the DOM behavior fixture and browser evidence.
export const assessment = {
  disposition: 'selectors', confidence: 'high', recommended_crew: 'reviewer', recommended_complexity: 'medium',
  assessment_rationale: 'Both presentation surfaces are available. Keep the stored assessment intact and show its operator decisions.',
  evidence_gaps: ['Confirm that the fixture preserves the raw receipt.'],
  reassessment_triggers: ['Reassess if the stored assessment fields change.'],
  blocked_by: ['Awaiting a dependency check'],
  duplicate_of: { task_id: 'ORB-42', evidence: 'Related task covers the same behavior.' },
  already_landed: { evidence: 'The existing presentation was verified on the base.' },
};
export const message = `operation_id=${'a'.repeat(64)}\n${JSON.stringify({
  assessment, crew_before: 'implementer', complexity_before: 'low', complexity_after: 'medium',
})}`;
export const comment = { by: 'task-pilot', at: '2026-10-08T08:00:00Z', message };
export const task = {
  id: 'ORB-77', title: 'Readable task pilot assessment', status: 'in-progress', priority: 'medium',
  description: 'A fixture showing a pilot assessment in the comment thread.', comments: [comment],
};
