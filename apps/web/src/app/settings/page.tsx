"use client";

import { useEffect, useState } from "react";
import { api, type NotificationPreferences } from "@/lib/api";

interface ToggleProps {
  checked: boolean;
  onChange: (val: boolean) => void;
  id: string;
  label: string;
  description?: string;
  disabled?: boolean;
}

function Toggle({ checked, onChange, id, label, description, disabled }: ToggleProps) {
  return (
    <label
      htmlFor={id}
      className="flex items-center justify-between gap-4 py-3 cursor-pointer"
    >
      <div className="flex-1">
        <span className="text-sm font-medium text-gray-900">{label}</span>
        {description && (
          <p className="text-xs text-gray-500 mt-0.5">{description}</p>
        )}
      </div>
      <button
        type="button"
        role="switch"
        id={id}
        aria-checked={checked}
        disabled={disabled}
        onClick={() => onChange(!checked)}
        className={`relative inline-flex h-6 w-11 shrink-0 rounded-full border-2 border-transparent transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-teal-600 focus-visible:ring-offset-2 disabled:opacity-50 ${
          checked ? "bg-teal-600" : "bg-gray-200"
        }`}
      >
        <span
          aria-hidden="true"
          className={`pointer-events-none inline-block h-5 w-5 rounded-full bg-white shadow transform transition-transform ${
            checked ? "translate-x-5" : "translate-x-0"
          }`}
        />
      </button>
    </label>
  );
}

type CategoryKey = "deadline" | "new_program" | "profile_update";
type ChannelKey = keyof NotificationPreferences["channels"];

export default function SettingsPage() {
  // Server state is the only source of truth: nothing is shown as saved
  // until GET returned it or PUT confirmed it.
  const [prefs, setPrefs] = useState<NotificationPreferences | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [saveError, setSaveError] = useState(false);

  useEffect(() => {
    api
      .getNotificationPreferences()
      .then((data) => setPrefs(data))
      .catch(() => setLoadError(true))
      .finally(() => setLoading(false));
  }, []);

  function updateCategory(key: CategoryKey, val: boolean) {
    setPrefs((prev) => (prev ? { ...prev, [key]: val } : prev));
    setSaved(false);
  }

  function updateChannel(key: ChannelKey, val: boolean) {
    setPrefs((prev) =>
      prev ? { ...prev, channels: { ...prev.channels, [key]: val } } : prev
    );
    setSaved(false);
  }

  async function handleSave() {
    if (!prefs || saving) return;
    setSaving(true);
    setSaveError(false);
    try {
      const confirmed = await api.saveNotificationPreferences(prefs);
      setPrefs(confirmed);
      setSaved(true);
    } catch {
      setSaveError(true);
    } finally {
      setSaving(false);
    }
  }

  const disabled = loading || loadError || !prefs;

  return (
    <main className="max-w-md mx-auto px-4 py-6 space-y-6">
      <div>
        <h1 className="text-xl font-bold text-gray-900">알림 설정</h1>
        <p className="text-sm text-gray-500 mt-1">
          받고 싶은 알림 유형을 선택하세요
        </p>
      </div>

      {loading && (
        <p className="text-sm text-gray-500 text-center" aria-live="polite">
          설정을 불러오는 중...
        </p>
      )}

      {loadError && (
        <p role="alert" className="text-sm text-red-600 text-center">
          알림 설정을 불러오지 못했어요. 로그인 상태를 확인한 뒤 다시 시도해 주세요.
        </p>
      )}

      {/* 카테고리별 토글 */}
      <section className="rounded-xl bg-white border border-gray-100 px-4 shadow-sm">
        <h2 className="text-xs font-semibold text-gray-400 uppercase tracking-wide pt-4 pb-1">
          알림 유형
        </h2>
        <div className="divide-y divide-gray-100">
          <Toggle
            id="toggle-deadline"
            checked={prefs?.deadline ?? false}
            disabled={disabled}
            onChange={(val) => updateCategory("deadline", val)}
            label="마감 임박"
            description="신청 마감 7일 전·3일 전·당일에 알림"
          />
          <Toggle
            id="toggle-new-program"
            checked={prefs?.new_program ?? false}
            disabled={disabled}
            onChange={(val) => updateCategory("new_program", val)}
            label="새 정책"
            description="내 프로필에 맞는 새 정책·장학금 등록 시 알림"
          />
          <Toggle
            id="toggle-profile-update"
            checked={prefs?.profile_update ?? false}
            disabled={disabled}
            onChange={(val) => updateCategory("profile_update", val)}
            label="프로필 변경"
            description="프로필 정보가 업데이트되면 알림"
          />
        </div>
      </section>

      {/* 알림 채널 */}
      <section className="rounded-xl bg-white border border-gray-100 px-4 shadow-sm">
        <h2 className="text-xs font-semibold text-gray-400 uppercase tracking-wide pt-4 pb-1">
          알림 채널
        </h2>
        <div className="divide-y divide-gray-100">
          <Toggle
            id="toggle-inapp"
            checked={prefs?.channels.in_app ?? false}
            disabled={disabled}
            onChange={(val) => updateChannel("in_app", val)}
            label="인앱 알림"
            description="앱 내 알림 벨을 통해 받기"
          />
          <Toggle
            id="toggle-email"
            checked={prefs?.channels.email ?? false}
            disabled={disabled}
            onChange={(val) => updateChannel("email", val)}
            label="이메일 알림"
            description="등록한 이메일로 받기"
          />
        </div>
      </section>

      {saveError && (
        <p role="alert" className="text-sm text-red-600 text-center">
          저장에 실패했습니다. 다시 시도해주세요.
        </p>
      )}

      {saved && !saveError && (
        <p className="text-sm text-green-600 text-center" aria-live="polite">
          설정이 저장되었습니다.
        </p>
      )}

      <button
        type="button"
        onClick={handleSave}
        disabled={disabled || saving}
        className="w-full rounded-xl bg-teal-600 py-3 text-sm font-medium text-white hover:bg-teal-700 transition-colors disabled:opacity-60"
      >
        {saving ? "저장 중..." : "저장하기"}
      </button>
    </main>
  );
}
