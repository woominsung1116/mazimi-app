import type { ApiProgram, ApplicationResource } from './api';
type Policy = NonNullable<ApiProgram['application_policy']>;

export function destinationForChannel(policy: Policy, channel: string | null, informationOnly = false) {
  if (informationOnly || (channel && !['online', 'unknown'].includes(channel))) {
    return policy.notice_destination ?? (['application','login_required'].includes(policy.destination?.kind ?? '') ? null : policy.destination);
  }
  return policy.destination;
}

export function handoffLabel(policy: Policy, channel: string | null): string {
  const destination = destinationForChannel(policy, channel);
  if (!destination) return '신청 준비 저장';
  if (destination.kind === 'application' || destination.kind === 'login_required') return destination.label;
  const offline = policy.workflow.channels.find(c => c.id === channel && !['online','unknown'].includes(c.id));
  return offline ? `준비 저장하고 ${offline.label} 안내 열기` : '준비 저장하고 공식 안내 열기';
}

export function currentResource(program: ApiProgram, previous: ApplicationResource, now = Date.now()): ApplicationResource {
  const resources = program.application_policy?.resources;
  const current = [resources?.notice, resources?.application, ...(resources?.templates ?? [])].find(r => r?.id === previous.id);
  if (!current || current.url !== previous.url || current.source_url !== previous.source_url || current.access !== previous.access || current.content_hash !== previous.content_hash || Date.parse(current.valid_until) <= now || !Number.isFinite(Date.parse(current.valid_until))) {
    throw new Error('자료가 변경됐거나 확인 기한이 지났어요. 새 공고를 확인해 주세요.');
  }
  const url = new URL(current.url);
  if (url.protocol !== 'https:' || url.username || url.password || (url.port && url.port !== '443')) throw new Error('공식 자료 주소를 다시 확인해 주세요.');
  return current;
}

/** "HWP · 원본 양식"; no empty part when the format is unknown. No verification date: that is internal. */
export function resourceMetaLine(resource: Pick<ApplicationResource, 'file_format'>): string {
  return [resource.file_format?.toUpperCase(), '원본 양식'].filter(Boolean).join(' · ');
}
