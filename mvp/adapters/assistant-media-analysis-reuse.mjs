import { createHash } from 'node:crypto';

const digest = value => typeof value === 'string' && /^[a-f0-9]{64}$/u.test(value);
const bounded = value => typeof value === 'string' && value.trim().length > 0 && value.length <= 512 && !/[\u0000-\u001f\u007f]/u.test(value);
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const exact = (value, keys) => object(value) && Object.keys(value).sort().join('\0') === [...keys].sort().join('\0');
const stable = value => JSON.stringify(value, (_, item) => object(item) ? Object.fromEntries(Object.entries(item).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)) : item);
const hash = value => createHash('sha256').update(stable(value)).digest('hex');
const attachmentIdentity = value => hash(['type','sourceUrl','source_url','url','id','canonicalMediaId'].map(key => typeof value?.[key] === 'string' ? value[key] : null));
const EDGE = ['schemaVersion','match','postKey','targetPostId','sourcePostKey','companyId','connectorBinding','target','verifiedFile','proofSha256','resultSha256','specSha256','normalizedOutput','transcript','originalSourceVersion','coverage','screenReuse'];
const TARGET = ['connectorBinding','postId','postKey','sourceVersion','attachmentIndex','attachmentIdentity','aliasRevision'];
const CONNECTOR = ['id','workspaceId','accountId','connector','revision','providerAccountId'];
const ref = value => exact(value, ['sha256','bytes']) && digest(value.sha256) && Number.isSafeInteger(value.bytes) && value.bytes > 0;
const invalid = () => { const failure = new Error('Invalid verified exact-file analysis binding'); failure.code = 'ASSISTANT_INVALID_REQUEST'; failure.requestCategory = 'MEDIA_ANALYSIS_REUSE'; throw failure; };

export function validateExactFileAnalysisBinding(edge, entry, material, posts, connector, account) {
  const target = Array.isArray(posts) ? posts.find(post => post.id === edge?.targetPostId && post.postKey === edge?.postKey) : undefined;
  const currentVersion = target?.preparationMediaPolicy?.sourceVersion ?? target?.mediaPolicy?.sourceVersion;
  const alias = edge?.target;
  const tr = material?.transcription;
  if (!exact(edge, EDGE) || edge.schemaVersion !== 1 || edge.match !== 'verified_exact_file_analysis_reuse'
    || edge.companyId !== account?.displayName || edge.screenReuse !== false
    || !['postKey','targetPostId','sourcePostKey'].every(key => bounded(edge[key]))
    || !['proofSha256','resultSha256','specSha256','originalSourceVersion'].every(key => digest(edge[key]))
    || !exact(connector, CONNECTOR) || !exact(edge.connectorBinding, CONNECTOR)
    || !['id','workspaceId','accountId','connector','providerAccountId'].every(key => bounded(connector[key]))
    || !Number.isSafeInteger(connector.revision) || connector.revision < 1
    || stable(edge.connectorBinding) !== stable(connector) || connector.accountId !== account.displayName
    || connector.providerAccountId !== account.providerAccountId
    || !exact(alias, TARGET) || stable(alias.connectorBinding) !== stable(connector)
    || alias.postId !== edge.targetPostId || alias.postKey !== edge.postKey || alias.sourceVersion !== currentVersion
    || !digest(alias.sourceVersion) || !digest(alias.attachmentIdentity)
    || !Number.isSafeInteger(alias.aliasRevision) || alias.aliasRevision < 1
    || !Number.isSafeInteger(alias.attachmentIndex) || alias.attachmentIndex < 0
    || !target || !Array.isArray(target.attachments) || !target.attachments[alias.attachmentIndex]
    || !['video','clip','reel'].includes(target.attachments[alias.attachmentIndex].type)
    || attachmentIdentity(target.attachments[alias.attachmentIndex]) !== alias.attachmentIdentity
    || !exact(edge.verifiedFile, ['sha256','bytes','receiptSha256','probeSha256'])
    || !digest(edge.verifiedFile.sha256) || !digest(edge.verifiedFile.receiptSha256) || !digest(edge.verifiedFile.probeSha256)
    || !Number.isSafeInteger(edge.verifiedFile.bytes) || edge.verifiedFile.bytes < 1 || !ref(edge.normalizedOutput)
    || !exact(edge.transcript, ['entryId','versionId','hash']) || !bounded(edge.transcript.entryId)
    || !bounded(edge.transcript.versionId) || !digest(edge.transcript.hash)
    || entry?.kind !== 'transcript' || material?.kind !== 'transcript' || !['source_only','verified'].includes(entry.trust)
    || material.trust !== entry.trust || material.account !== account.displayName || material.postKey !== edge.sourcePostKey
    || material.mediaSha256 !== edge.verifiedFile.sha256 || typeof material.text !== 'string' || !material.text.trim()
    || material.knowledgeEntryId !== entry.entryId || material.knowledgeVersionId !== entry.versionId
    || edge.transcript.entryId !== entry.entryId || edge.transcript.versionId !== entry.versionId || edge.transcript.hash !== entry.hash
    || !tr || tr.partial !== false || tr.sourceVersion !== edge.originalSourceVersion || tr.sourcePostKey !== edge.sourcePostKey
    || !exact(edge.coverage, ['kind','durationMs']) || !Number.isSafeInteger(edge.coverage.durationMs) || edge.coverage.durationMs < 1
    || typeof tr.mediaDurationSeconds !== 'number' || !Number.isFinite(tr.mediaDurationSeconds) || tr.mediaDurationSeconds <= 0
    || Math.abs(Math.round(tr.mediaDurationSeconds * 1000) - edge.coverage.durationMs) > 250) invalid();
  const speech = edge.coverage.kind === 'full_audio' && tr.coverage === 'full_audio'
    && typeof tr.audioDurationSeconds === 'number' && Number.isFinite(tr.audioDurationSeconds)
    && tr.audioDurationSeconds > 0 && tr.audioDurationSeconds + 0.25 >= tr.mediaDurationSeconds;
  const silent = edge.coverage.kind === 'no_audio_stream' && tr.coverage === 'no_audio_stream'
    && tr.audioStatus === 'no_audio_stream' && tr.audioDurationSeconds === null;
  if (!speech && !silent) invalid();
  return structuredClone(edge);
}

export function projectMaterialExactFileAnalysis(material, manifest, posts, connector, account) {
  if (material.exactFileAnalysisReuse === undefined) return undefined;
  if (!Array.isArray(material.exactFileAnalysisReuse) || material.exactFileAnalysisReuse.length < 1 || material.exactFileAnalysisReuse.length > 100) invalid();
  if (!Array.isArray(manifest) || manifest.length > 300) invalid();
  const entry = manifest.find(row => row.entryId === material.knowledgeEntryId && row.versionId === material.knowledgeVersionId && row.kind === 'transcript');
  if (!entry) invalid();
  return material.exactFileAnalysisReuse.map(edge => {
    if (!Array.isArray(entry.mediaBinding) || !entry.mediaBinding.some(binding => stable(binding) === stable(edge))) invalid();
    return validateExactFileAnalysisBinding(edge, entry, material, posts, connector, account);
  });
}
