// Called only after assistant.mjs has validated the complete stored evidence.
// The model needs observations and their video offsets, while the frame and
// receipt hashes remain in the validated source request for audit/recovery.
export function projectValidatedVisualEvidence(value) {
  if (value.schemaVersion === 2) {
    return {
      schemaVersion: 2,
      modelProjectionVersion: 1,
      source: value.source,
      sourcePostVersion: value.sourcePostVersion,
      evidenceSha256: value.finalEvidence.sha256,
      coverage: {
        kind: value.coverage.kind,
        frameCount: value.coverage.frameCount,
        selectedFrameCount: value.coverage.selectedFrameCount,
        coveredSelectedFrameCount: value.coverage.coveredSelectedFrameCount,
        uniqueReviewedFrames: value.coverage.uniqueReviewedFrames
      },
      aggregateOverflow: value.aggregateOverflow,
      aggregate: value.aggregate.map((group, index) => ({
        id: `group-${index + 1}`,
        observation: group.observation,
        sourceTimestampsMs: group.sources.map(frame => frame.timestampMs)
      }))
    };
  }
  return {
    schemaVersion: 1,
    modelProjectionVersion: 1,
    source: value.source,
    evidenceSha256: value.durableManifestSha256,
    coverage: value.coverage,
    frames: value.frames.map(frame => ({
      id: frame.id,
      timestampMs: frame.timestampMs,
      status: frame.status,
      scene: frame.scene,
      text: frame.text,
      numbers: frame.numbers,
      uncertainties: frame.uncertainties
    }))
  };
}
