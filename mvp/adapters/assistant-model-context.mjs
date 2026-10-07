// Model-only projection of ALREADY validated evidence. The caller keeps its
// complete payload for image staging, admission, source bindings and audit.
// Never use this projection to validate a request or to compute source identity.
import {projectModerationRuleSets} from './assistant-moderation-context.mjs';
const POST_MEDIA_KINDS = new Set(['transcript', 'ocr', 'visual_context']);
const rows = value => Array.isArray(value) ? value : [];
const present = value => typeof value === 'string' && value.length > 0;
const versionKey = value => present(value.knowledgeEntryId ?? value.entryId)
  && present(value.knowledgeVersionId ?? value.versionId)
  ? JSON.stringify([value.knowledgeEntryId ?? value.entryId, value.knowledgeVersionId ?? value.versionId]) : null;

function reviewSources(payload) {
  if (payload.purpose !== 'triage_review' || !rows(payload.items).length) return payload;
  const items = payload.items, branches = rows(payload.branches), posts = rows(payload.posts);
  const itemIds = new Set(items.map(item => item.id));
  const branchIds = new Set(items.map(item => item.branchId).filter(present));
  const postIds = new Set(items.map(item => item.postId).filter(present));
  const postKeys = new Set(items.map(item => item.postKey).filter(present));
  // A legacy request without resolvable source pointers cannot prove that any
  // supplied context is unrelated. Keep it rather than guess from text/URLs.
  if (items.some(item => !present(item.branchId) && !present(item.postId) && !present(item.postKey))) return payload;
  if (items.some(item => present(item.branchId) && !branches.some(branch => branch.id === item.branchId))) return payload;
  // A direct post pointer is no stronger than an unresolved branch/key pointer.
  // If neither a supplied post nor a resolved branch locates this recipient's
  // post, absence cannot prove that the other attached evidence is unrelated.
  if (items.some(item => present(item.postId) && !posts.some(post => post.id === item.postId)
    && !posts.some(post => present(item.postKey) && post.postKey === item.postKey)
    && !branches.some(branch => branch.id === item.branchId && present(branch.postId)
      && posts.some(post => post.id === branch.postId)))) return payload;
  const unassignedItems = items.filter(item => !present(item.branchId));
  const unassignedPosts = new Set(unassignedItems.map(item => item.postId).filter(present));
  for (const item of unassignedItems) if (present(item.postKey)) {
    const matched = posts.filter(post => post.postKey === item.postKey);
    if (!matched.length && !present(item.postId)) return payload;
    for (const post of matched) if (present(post.id)) unassignedPosts.add(post.id);
  }
  const selectedBranches = branches.filter(branch => branchIds.has(branch.id) || unassignedPosts.has(branch.postId));
  for (const branch of selectedBranches) if (present(branch.postId)) postIds.add(branch.postId);
  for (const post of posts) if (postIds.has(post.id) || postKeys.has(post.postKey)) {
    if (present(post.id)) postIds.add(post.id);
    if (present(post.postKey)) postKeys.add(post.postKey);
  }
  const bindingsMatch = edges => rows(edges).some(edge => postKeys.has(edge.postKey) || postIds.has(edge.targetPostId));
  const knownPostKeys = new Set(posts.map(post => post.postKey).filter(present));
  const manifests = rows(payload.knowledgeManifest);
  const materials = rows(payload.materials).filter(material => {
    // Company rules, facts, research, references and unknown material kinds are
    // deliberately retained. Their applicability is owned by canonical policy.
    if (rows(material.itemIds).some(id => itemIds.has(id))) return true;
    if (!POST_MEDIA_KINDS.has(material.kind) || !present(material.postKey) || postKeys.has(material.postKey)
      || !knownPostKeys.has(material.postKey)) return true;
    if (bindingsMatch(material.audioEquivalence)) return true;
    const key = versionKey(material);
    return key !== null && manifests.some(entry => versionKey(entry) === key && bindingsMatch(entry.mediaBinding));
  });
  const keptVersions = new Set(materials.map(versionKey).filter(present));
  const removedVersions = new Set(rows(payload.materials).filter(material => !materials.includes(material)).map(versionKey).filter(present));
  const knowledgeManifest = manifests.filter(entry => !removedVersions.has(versionKey(entry)) || keptVersions.has(versionKey(entry)));
  // Keep explicitly referenced donor/target posts of retained evidence too.
  // Audio equivalence stays directional; no transfer of donor visuals is inferred.
  for (const source of [...materials, ...knowledgeManifest]) {
    for (const edge of [source, ...rows(source.audioEquivalence), ...rows(source.mediaBinding)]) {
      for (const field of ['sourcePostId', 'targetPostId']) if (present(edge[field])) postIds.add(edge[field]);
      for (const field of ['postKey', 'sourcePostKey']) if (present(edge[field])) postKeys.add(edge[field]);
    }
  }
  return {...payload, branches:selectedBranches,
    posts:posts.filter(post => postIds.has(post.id) || postKeys.has(post.postKey)), materials, knowledgeManifest};
}

function withoutExactCopy(record, duplicate, source) {
  if (typeof record[source] !== 'string' || record[duplicate] !== record[source]) return record;
  const result = {...record};
  delete result[duplicate];
  return result;
}

function isValidatedVideoFrameMaterial(material) {
  const evidence = material.visualEvidence;
  // Existing V1/V2 contracts are sampled-video-frame evidence with a positive
  // duration and exact source binding. A bare kind/post association is not
  // proof of modality: preserve photo-derived and unclassified descriptions.
  return material.kind === 'visual_context' && evidence?.modelProjectionVersion === 1
    && Number.isSafeInteger(evidence.source?.durationMs) && evidence.source.durationMs > 0
    && present(material.postKey) && evidence.source.postKey === material.postKey
    && ((evidence.schemaVersion === 1 && evidence.coverage?.kind === 'sampled_frames')
      || (evidence.schemaVersion === 2 && evidence.coverage?.kind === 'all_frames_fast_selected_neural'));
}

function videoTextOnly(payload) {
  const omitted = rows(payload.materials).filter(isValidatedVideoFrameMaterial);
  const video = omitted.length > 0 || rows(payload.posts).some(post => post.mediaPolicy
    || rows(post.attachments).some(attachment => attachment.type === 'video'));
  if (!video) return payload;
  // Owner policy: ordinary video context is speech and actually extracted
  // screen text only. Do not substitute visual material.text: it may be a scene
  // summary or merely bookkeeping, and is not an OCR/transcript result.
  const materials = rows(payload.materials).filter(material => !omitted.includes(material));
  const omittedVersions = new Set(omitted.map(versionKey).filter(present));
  const keptVersions = new Set(materials.map(versionKey).filter(present));
  const result = {...payload,
    videoEvidenceProjection: {
      version:1,
      mode:'speech_and_extracted_screen_text_only',
      scope:'validated_video_frame_sources',
      frameObservationsProvided:false,
      sceneSummariesProvided:false,
      missingScreenTextStatus:'not_provided',
      missingScreenTextMeaning:'Absent OCR evidence does not establish whether extraction ran, found no text, or failed. Do not invent captions or infer that the video has no on-screen text.',
      sourceReadinessMeaning:'Media policy describes canonical source admission, not visual evidence supplied to this model. Validated video frame observations and scene summaries were omitted. Retained unclassified visual material must not be assumed to describe video.',
      visualQuestionPolicy:'Use supplied speech, extracted screen text, or directly attached photos only when their source binding supports the question. If the answer requires unseen video detail, return needs_attention with missing_context; do not guess. A stored proof or prior model answer does not establish an unseen detail.'
    }
  };
  if(payload.mandatoryMaterialContract==='mandatory_post_materials_v1')Object.assign(result.videoEvidenceProjection,{
    mode:'complete_speech_and_explicit_requested_frames',
    frameObservationsProvided:payload.targetedVideoFrameEvidence?.status==='attached',
    screenTextPolicy:'Existing OCR entries are retained read-only source facts with their recorded sampled coverage. This request performs no OCR or subtitle extraction and does not claim full screen-text coverage.',
    visualQuestionPolicy:'Every photo of each selected post is directly attached. Complete speech outcomes distinguish transcript, inspected no_speech and proven no_audio. If a held answer needs an unseen video detail, return a bounded videoFrameNeeds request; do not infer video pixels from a source receipt or old scene summary.'
  });
  if (Array.isArray(payload.materials)) result.materials = materials;
  if (Array.isArray(payload.knowledgeManifest)) result.knowledgeManifest = payload.knowledgeManifest.filter(entry =>
    !omittedVersions.has(versionKey(entry)) || keptVersions.has(versionKey(entry)));
  const videoPostKeys = new Set(omitted.map(material => material.postKey));
  if (Array.isArray(payload.posts)) result.posts = payload.posts.map(post => {
    if (!['complete','missing'].includes(post.visualContextStatus)
      || !(post.mediaPolicy || videoPostKeys.has(post.postKey) || rows(post.attachments).some(attachment => attachment.type === 'video'))) return post;
    const projected = {...post};
    // Canonical readiness is retained in the full payload, but cannot imply
    // that the model read visual observations deliberately omitted here.
    delete projected.visualContextStatus;
    return projected;
  });
  return result;
}

export function projectValidatedModelContext(payload) {
  const scoped = projectModerationRuleSets(videoTextOnly(reviewSources(payload)));
  // These are exact same-record copies, never a summary or fuzzy match. Keep
  // differing previews/bodies, transcript uncertainty and all image evidence.
  const compact = {...scoped};
  if (Array.isArray(scoped.items)) compact.items = scoped.items.map(item => withoutExactCopy(item, 'preview', 'text'));
  if (Array.isArray(scoped.posts)) compact.posts = scoped.posts.map(post => withoutExactCopy(post, 'body', 'text'));
  // Stable shared context precedes per-attempt and recipient fields. This is
  // cache-friendly ordering only; it does not claim a provider cache hit.
  const ordered = {};
  for (const key of ['account', 'materials', 'knowledgeManifest', 'knowledgePolicyVersion', 'posts', 'branches']) {
    if (Object.hasOwn(compact, key)) ordered[key] = compact[key];
  }
  for (const [key, value] of Object.entries(compact)) if (!Object.hasOwn(ordered, key)) ordered[key] = value;
  return ordered;
}

export const serializeAssistantModelInput = payload => JSON.stringify(projectValidatedModelContext(payload));
