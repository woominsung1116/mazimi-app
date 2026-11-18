import { BulletRow, FigmaToggle, FigmaIcon, NotificationBell, ScreenHeader, SearchBar, ui } from "@/components/FigmaUI";
import { useConfirmDialog } from "@/components/ConfirmDialog";
import MyDocumentItems from "@/components/MyDocumentItems";
import Tappable from "@/components/Tappable";
import { useDocumentItemsSupport } from "@/lib/catalogue";
import { useMemberAction } from "@/components/MemberAction";
/**
 * 디지털 서류 보관함 — /document-vault
 *
 * Storage strategy (on-device only, by design):
 *   - Metadata: AsyncStorage, per-account key from lib/vault.ts
 *   - Files: expo-file-system (app-local), AES-256-CBC + HMAC via lib/crypto.ts;
 *     the plaintext is deleted after encryption.
 *   - Documents never leave this device. There is no server upload route; the
 *     user issues documents themselves and submits them at the official site.
 *
 * Features:
 *   1. 서류 목록 — name, issued_at, expires_at, status badge, type icon/color
 *   2. 서류 추가 — camera (expo-image-picker), gallery/PDF (expo-document-picker),
 *      document type selector
 *   3. 만료 알림 — 발급일로부터 90일 경과 시 "갱신 필요" badge + 발급처 링크
 *
 * Design: Stitch tokens, No-Line Rule (background tonal shifts only, no 1px borders
 * for structural sectioning).
 */

import * as Sharing from "expo-sharing";
import { useQueryClient } from "@tanstack/react-query";
import React, { useCallback, useEffect, useRef, useState } from "react";
import {
  ActionSheetIOS,
  ActivityIndicator,
  Alert,
  Animated,
  FlatList,
  Image,
  Linking,
  Modal,
  Platform,
  Pressable,
  ScrollView,
  StyleSheet,
  
  useWindowDimensions,
  View,
} from "react-native";
import Text from "@/components/Text";
import { useFocusEffect, useRouter } from "expo-router";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import * as ImagePicker from "expo-image-picker";
// expo-image-manipulator may not be available in Expo Go
let ImageManipulator: any = null;
try {
  ImageManipulator = require("expo-image-manipulator");
} catch {
  // Not available — scan preview will skip processing
}
import * as DocumentPicker from "expo-document-picker";
import * as FileSystem from "expo-file-system/legacy";
import Constants from "expo-constants";
import { addVaultEntry, decryptVaultFile, loadVaultDocuments, storeVaultFile, updateVault, vaultGeneration, VaultFileMissingError, type DocumentType, type StoredDocument, type VaultEntry } from "@/lib/vault";
import {
  colors,
  typography,
  spacing,
  borderRadius,
  shadows,
  layout,
  fig,
} from "@/constants/theme";
import { useAuthStore } from "@/store/auth";
import { useConsentStore } from "@/store/consent";
import { AgreementCheckbox } from "@/components/AgreementCheckbox";
import { requireLogin } from "@/lib/navigation";
import { notify } from "@/lib/notify";
import { APP_INSTALL_URL } from "@/constants/links";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

// VAULT_STORAGE_KEY는 @/lib/vault의 VAULT_VAULT_STORAGE_KEY로 통합됨

/** Days after issuedAt before we show "갱신 필요" */
const RENEWAL_THRESHOLD_DAYS = 90;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type { DocumentType, StoredDocument } from "@/lib/vault";

export type DocumentStatus = "valid" | "expiring_soon" | "renewal_needed" | "expired";

// ---------------------------------------------------------------------------
// Document type config
// ---------------------------------------------------------------------------

interface DocTypeConfig {
  label: string;
  shortLabel: string;
  iconText: string;
  accentColor: string;
  backgroundColor: string;
  issuerLabel: string;
  issuerUrl: string;
}

const DOC_TYPE_CONFIG: Record<DocumentType, DocTypeConfig> = {
  resident_register: {
    label: "주민등록등본",
    shortLabel: "등본",
    iconText: "ID",
    accentColor: "#34BB84",
    backgroundColor: "#2A8F6614",
    issuerLabel: "정부24",
    issuerUrl: "https://www.gov.kr/mw/AA020InfoCappView.do?HighCtgCD=A01001&CappBizCD=13100000015",
  },
  income_proof: {
    label: "소득증명원",
    shortLabel: "소득",
    iconText: "W",
    accentColor: "#239969",
    backgroundColor: "#2A8F6614",
    issuerLabel: "홈택스",
    issuerUrl: "https://www.hometax.go.kr",
  },
  enrollment_cert: {
    label: "재학증명서",
    shortLabel: "재학",
    iconText: "EDU",
    accentColor: "#0095FF",
    backgroundColor: "#0095FF14",
    issuerLabel: "학교 포털",
    issuerUrl: "",
  },
  leave_cert: {
    label: "휴학증명서",
    shortLabel: "휴학",
    iconText: "EDU",
    accentColor: "#F75B2B",
    backgroundColor: "#F72B2B0A",
    issuerLabel: "학교 포털",
    issuerUrl: "",
  },
  graduation_cert: {
    label: "졸업증명서",
    shortLabel: "졸업",
    iconText: "GRD",
    accentColor: "#239969",
    backgroundColor: "#2A8F6614",
    issuerLabel: "학교 포털",
    issuerUrl: "",
  },
  health_insurance: {
    label: "건강보험료 납부확인서",
    shortLabel: "건보",
    iconText: "MED",
    accentColor: "#191A54",
    backgroundColor: "#0095FF14",
    issuerLabel: "건강보험공단",
    issuerUrl: "https://www.nhis.or.kr",
  },
  family_relation: {
    label: "가족관계증명서",
    shortLabel: "가족",
    iconText: "FAM",
    accentColor: "#34BB84",
    backgroundColor: "#2A8F6614",
    issuerLabel: "정부24",
    issuerUrl: "https://www.gov.kr",
  },
  bank_statement: {
    label: "통장사본",
    shortLabel: "통장",
    iconText: "BNK",
    accentColor: "#0095FF",
    backgroundColor: "#0095FF14",
    issuerLabel: "인터넷뱅킹",
    issuerUrl: "",
  },
  disability_cert: {
    label: "장애인증명서",
    shortLabel: "장애",
    iconText: "DIS",
    accentColor: "#5D5D62",
    backgroundColor: "#2A8F6614",
    issuerLabel: "정부24",
    issuerUrl: "https://www.gov.kr",
  },
  other: {
    label: "기타 서류",
    shortLabel: "기타",
    iconText: "DOC",
    accentColor: "#8894A1",
    backgroundColor: "#E8E8E8",
    issuerLabel: "발급처 확인 필요",
    issuerUrl: "",
  },
};

// ---------------------------------------------------------------------------
// RRN (주민등록번호) masking guidance — safety guardrail
//
// Only document types that commonly carry a resident registration number on
// the official issuance are covered here. Types not listed (재학증명서,
// 통장사본 등) don't get the masking guidance/checkbox at all — adding it
// there would be friction with no legal basis.
// ---------------------------------------------------------------------------

interface RrnMaskingInfo {
  /** true = 정부24 등 발급 화면 기본값이 "전체표시"라 유저가 직접 비공개를 선택해야 함 */
  defaultsToFullDisplay: boolean;
  guidance: string;
}

const RRN_MASKING_INFO: Partial<Record<DocumentType, RrnMaskingInfo>> = {
  resident_register: {
    defaultsToFullDisplay: true,
    guidance:
      "정부24 발급 화면에서 \"주민등록번호 뒷자리 표시\" 옵션을 반드시 '비공개'로 선택하세요. 기본값은 '전체표시'입니다.",
  },
  family_relation: {
    defaultsToFullDisplay: true,
    guidance:
      "정부24 발급 화면에서 \"주민등록번호 뒷자리 표시\" 옵션을 반드시 '비공개'로 선택하세요. 기본값은 '전체표시'입니다.",
  },
  health_insurance: {
    defaultsToFullDisplay: false,
    guidance:
      "건강보험공단 발급 서류는 기본적으로 주민등록번호 뒷자리가 마스킹되어 나와요. 그래도 발급 화면에서 마스킹 여부를 한 번 더 확인하세요.",
  },
};

const DOCUMENT_TYPE_OPTIONS: DocumentType[] = [
  "resident_register",
  "income_proof",
  "enrollment_cert",
  "leave_cert",
  "graduation_cert",
  "health_insurance",
  "family_relation",
  "bank_statement",
  "disability_cert",
  "other",
];

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function computeStatus(doc: StoredDocument): DocumentStatus {
  const now = Date.now();

  if (doc.expiresAt) {
    const expiry = new Date(doc.expiresAt).getTime();
    if (expiry < now) return "expired";
    if (expiry - now < 30 * 24 * 60 * 60 * 1000) return "expiring_soon";
  }

  const issued = new Date(doc.issuedAt).getTime();
  const daysSince = (now - issued) / (1000 * 60 * 60 * 24);
  if (daysSince >= RENEWAL_THRESHOLD_DAYS) return "renewal_needed";

  return doc.fileUri ? "valid" : "renewal_needed";
}

function formatDate(iso: string): string {
  const d = new Date(iso);
  return `${d.getFullYear()}.${String(d.getMonth() + 1).padStart(2, "0")}.${String(d.getDate()).padStart(2, "0")}`;
}

// ---------------------------------------------------------------------------
// Scan preview modal
// ---------------------------------------------------------------------------

interface ScanPreviewModalProps {
  /** Raw URI from the camera, or null when closed */
  rawUri: string | null;
  onRetake: () => void;
  onConfirm: (processedUri: string) => void;
  /** iOS only: fires after the modal has fully closed */
  onDismiss?: () => void;
}

function ScanPreviewModal({ rawUri, onRetake, onConfirm, onDismiss }: ScanPreviewModalProps) {
  const [grayscale, setGrayscale] = useState(false);
  const [isProcessing, setIsProcessing] = useState(false);
  const [processedUri, setProcessedUri] = useState<string | null>(null);

  // Re-process whenever rawUri or grayscale changes
  useEffect(() => {
    if (!rawUri) {
      setProcessedUri(null);
      return;
    }

    let cancelled = false;
    setIsProcessing(true);

    (async () => {
      try {
        let result: { uri: string };
        if (ImageManipulator) {
          const actions: any[] = [{ resize: { width: 1500 } }];
          result = await ImageManipulator.manipulateAsync(
            rawUri,
            actions,
            {
              compress: 0.8,
              format: ImageManipulator.SaveFormat?.JPEG ?? 'jpeg',
            }
          );
        } else {
          // Fallback: use raw image without processing (Expo Go)
          result = { uri: rawUri };
        }

        if (!cancelled) {
          // If grayscale is requested and the library doesn't natively support it,
          // we mark the URI as-is — a proper grayscale pass would require a canvas
          // or native module beyond expo-image-manipulator's current API surface.
          setProcessedUri(result.uri);
        }
      } catch {
        if (!cancelled) setProcessedUri(rawUri);
      } finally {
        if (!cancelled) setIsProcessing(false);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [rawUri, grayscale]);

  // Stays mounted while closed so iOS can report onDismiss after the slide-out.
  return (
    <Modal
      visible={!!rawUri}
      transparent={false}
      animationType="slide"
      statusBarTranslucent
      onDismiss={onDismiss}
    >
      <View style={scanStyles.root}>
        {/* Header */}
        <View style={scanStyles.header}>
          <Text style={scanStyles.headerTitle}>스캔 결과 미리보기</Text>
        </View>

        {/* Preview area */}
        <View style={scanStyles.previewArea}>
          {isProcessing ? (
            <View style={scanStyles.processingWrap}>
              <ActivityIndicator size="large" color={colors.primary} />
              <Text style={scanStyles.processingText}>이미지 처리 중...</Text>
            </View>
          ) : processedUri ? (
            <Image
              source={{ uri: processedUri }}
              style={scanStyles.previewImage}
              resizeMode="contain"
            />
          ) : null}
        </View>

        {/* Options */}
        <View style={scanStyles.options}>
          <Text style={scanStyles.optionLabel}>흑백 변환</Text>
          <FigmaToggle value={grayscale} onValueChange={setGrayscale} accessibilityLabel="흑백 변환 토글" />
        </View>


        {/* Actions */}
        <View style={scanStyles.actions}>
          <Tappable feedback="opacity"
            style={scanStyles.retakeBtn}
            onPress={onRetake}
            accessibilityRole="button"
            accessibilityLabel="다시 찍기"
          >
            <Text style={scanStyles.retakeBtnText}>다시 찍기</Text>
          </Tappable>
          <Tappable feedback="opacity"
            style={[
              scanStyles.saveBtn,
              (isProcessing || !processedUri) && scanStyles.saveBtnDisabled,
            ]}
            onPress={() => {
              if (processedUri) onConfirm(processedUri);
            }}
            disabled={isProcessing || !processedUri}
            accessibilityRole="button"
            accessibilityLabel="저장하기"
            accessibilityState={{ disabled: isProcessing || !processedUri }}
          >
            <Text style={scanStyles.saveBtnText}>저장하기</Text>
          </Tappable>
        </View>
      </View>
    </Modal>
  );
}

const scanStyles = StyleSheet.create({
  root: {
    flex: 1,
    backgroundColor: "#FFFFFF",
    paddingTop: 56,
  },
  header: {
    paddingHorizontal: spacing[5],
    paddingBottom: spacing[4],
    backgroundColor: colors.surfaceContainerLowest,
    ...Platform.select({
      ios: {
        shadowColor: "#000",
        shadowOffset: { width: 0, height: 2 },
        shadowOpacity: 0.07,
        shadowRadius: 6,
      },
      android: { elevation: 3 },
    }),
  },
  headerTitle: {
    fontSize: typography.fontSize.md, lineHeight: 24,
    fontWeight: typography.fontWeight.bold,
    color: colors.onSurface,
    textAlign: "center",
  },
  previewArea: {
    flex: 1,
    margin: spacing[5],
    backgroundColor: colors.surfaceContainerLowest,
    borderRadius: borderRadius.xl,
    overflow: "hidden",
    alignItems: "center",
    justifyContent: "center",
    ...shadows.cardMd,
  },
  previewImage: {
    width: "100%",
    height: "100%",
  },
  processingWrap: {
    alignItems: "center",
    gap: spacing[3],
  },
  processingText: {
    ...typography.styles.bodySm,
    color: colors.textSecondary,
  },
  options: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    marginHorizontal: spacing[5],
    marginBottom: spacing[2],
    backgroundColor: colors.surfaceContainerLowest,
    borderRadius: borderRadius.lg,
    paddingHorizontal: spacing[5],
    paddingVertical: spacing[3.5],
    ...shadows.card,
  },
  optionLabel: {
    ...typography.styles.label,
    color: colors.onSurface,
  },
  actions: {
    flexDirection: "row",
    gap: spacing[3],
    marginHorizontal: spacing[5],
    marginBottom: spacing[8],
  },
  retakeBtn: {
    flex: 1,
    height: layout.buttonHeightMd,
    backgroundColor: colors.surfaceContainerHigh,
    borderRadius: borderRadius.lg,
    alignItems: "center",
    justifyContent: "center",
  },
  retakeBtnText: {
    ...typography.styles.buttonLabelSm,
    color: colors.onSurfaceVariant,
  },
  saveBtn: {
    flex: 2,
    height: layout.buttonHeightMd,
    backgroundColor: colors.primary,
    borderRadius: borderRadius.lg,
    alignItems: "center",
    justifyContent: "center",
    ...shadows.primaryButton,
  },
  saveBtnDisabled: {
    backgroundColor: colors.secondaryContainer,
    shadowOpacity: 0,
    elevation: 0,
  },
  saveBtnText: {
    ...typography.styles.buttonLabel,
    color: colors.onPrimary,
  },
});

// ---------------------------------------------------------------------------
// Masking guidance card — 주민번호 마스킹 발급 가이드 (C)
//
// Shown as soon as the user picks a document type known to carry a resident
// registration number, so they see the warning *before* they go get the
// physical/digital document — not just after the fact.
// ---------------------------------------------------------------------------

function MaskingGuidanceCard({ type }: { type: DocumentType }) {
  const info = RRN_MASKING_INFO[type];
  const cfg = DOC_TYPE_CONFIG[type];
  if (!info) return null;

  return (
    <View style={maskingStyles.card}>
      <View style={maskingStyles.headerRow}>
        <FigmaIcon name={info.defaultsToFullDisplay ? "info" : "checkCircle"} size={16} color={info.defaultsToFullDisplay ? "#F75B2B" : cfg.accentColor} />
        <Text style={maskingStyles.headerText}>
          {info.defaultsToFullDisplay ? "발급 시 꼭 확인하세요" : "발급 안내"}
        </Text>
      </View>
      <Text style={maskingStyles.guidanceText}>{info.guidance}</Text>
      {cfg.issuerUrl ? (
        <Tappable feedback="opacity"
          style={maskingStyles.issuerLink}
          onPress={() => Linking.openURL(cfg.issuerUrl)}
          accessibilityRole="link"
          accessibilityLabel={`${cfg.issuerLabel}에서 발급받기`}
          hitSlop={{ top: 4, bottom: 4, left: 4, right: 4 }}
        >
          <Text style={maskingStyles.issuerLinkText}>{cfg.issuerLabel}에서 발급받기</Text>
          <FigmaIcon name="link" size={12} color={colors.primary} />
        </Tappable>
      ) : null}
    </View>
  );
}

const maskingStyles = StyleSheet.create({
  card: {
    backgroundColor: "#F72B2B0A",
    borderRadius: borderRadius.lg,
    padding: spacing[3.5],
    gap: spacing[1.5],
    marginTop: spacing[1],
  },
  headerRow: {
    flexDirection: "row",
    alignItems: "center",
    gap: spacing[1.5],
  },
  headerText: {
    fontSize: typography.fontSize.sm, lineHeight: 20,
    fontWeight: typography.fontWeight.semibold,
    color: "#F75B2B",
  },
  guidanceText: {
    fontSize: typography.fontSize.xs,
    color: "#F75B2B",
    lineHeight: 16,
  },
  issuerLink: {
    flexDirection: "row",
    alignItems: "center",
    gap: spacing[1],
    marginTop: spacing[0.5],
  },
  issuerLinkText: {
    fontSize: typography.fontSize.xs, lineHeight: 16,
    fontWeight: typography.fontWeight.semibold,
    color: colors.primary,
    textDecorationLine: "underline",
  },
});

// ---------------------------------------------------------------------------
// Health insurance sensitive-info consent modal (B)
//
// Triggered only when the user tries to attach an actual file while
// "health_insurance" is the selected type, and only if that consent hasn't
// already been recorded. Declining this modal must NOT block any other
// document type — the caller simply keeps the file picker closed.
// ---------------------------------------------------------------------------

interface HealthInsuranceConsentModalProps {
  visible: boolean;
  onCancel: () => void;
  onAgree: () => void;
  /** iOS only: fires after the modal has fully closed */
  onDismiss?: () => void;
}

function HealthInsuranceConsentModal({
  visible,
  onCancel,
  onAgree,
  onDismiss,
}: HealthInsuranceConsentModalProps) {
  const [checked, setChecked] = useState(false);

  useEffect(() => {
    if (!visible) setChecked(false);
  }, [visible]);

  return (
    <Modal visible={visible} transparent animationType="fade" onRequestClose={onCancel} onDismiss={onDismiss}>
      <View style={healthConsentStyles.overlay}>
        <View style={healthConsentStyles.card}>
          <View style={healthConsentStyles.iconWrap}>
            <FigmaIcon name="document" size={22} color={colors.primary} />
          </View>
          <Text style={healthConsentStyles.title}>건강보험 서류 처리 동의</Text>
          <Text style={healthConsentStyles.body}>
            건강보험 자격득실확인서는 건강 상태를 유추할 수 있는 민감정보로 분류되어
            다른 서류와 별도의 동의가 필요해요.
          </Text>
          <View style={healthConsentStyles.detailBox}>
            <BulletRow style={healthConsentStyles.detailLine} color={colors.textSecondary}>목적: 건강보험 관련 혜택 매칭 및 서류 보관</BulletRow>
            <BulletRow style={healthConsentStyles.detailLine} color={colors.textSecondary}>처리: 기기 내 암호화 저장, 서버 전송 없음</BulletRow>
            <BulletRow style={healthConsentStyles.detailLine} color={colors.textSecondary}>
              거부 시: 이 서류만 보관할 수 없어요. 등본·소득증명 등 다른 서류 보관 기능은
              그대로 이용할 수 있어요.
            </BulletRow>
          </View>

          <View style={healthConsentStyles.checkboxWrap}>
            <AgreementCheckbox
              checked={checked}
              onToggle={() => setChecked((v) => !v)}
              label="민감정보(건강보험 서류) 처리에 동의합니다"
              requirement="required"
            />
          </View>

          <View style={healthConsentStyles.actions}>
            <Tappable feedback="opacity"
              style={healthConsentStyles.cancelBtn}
              onPress={onCancel}
              accessibilityRole="button"
              accessibilityLabel="동의하지 않음"
            >
              <Text style={healthConsentStyles.cancelBtnText}>동의하지 않음</Text>
            </Tappable>
            <Tappable feedback="opacity"
              style={[
                healthConsentStyles.agreeBtn,
                !checked && healthConsentStyles.agreeBtnDisabled,
              ]}
              onPress={() => {
                if (checked) onAgree();
              }}
              disabled={!checked}
              accessibilityRole="button"
              accessibilityLabel="동의하고 계속"
              accessibilityState={{ disabled: !checked }}
            >
              <Text style={healthConsentStyles.agreeBtnText}>동의하고 계속</Text>
            </Tappable>
          </View>
        </View>
      </View>
    </Modal>
  );
}

const healthConsentStyles = StyleSheet.create({
  overlay: {
    flex: 1,
    backgroundColor: colors.overlay,
    alignItems: "center",
    justifyContent: "center",
    padding: spacing[6],
  },
  card: {
    width: "100%",
    maxWidth: 400,
    backgroundColor: colors.surfaceContainerLowest,
    borderRadius: borderRadius.xl,
    padding: spacing[6],
    gap: spacing[3],
    ...shadows.floating,
  },
  iconWrap: {
    width: 44,
    height: 44,
    borderRadius: borderRadius.lg,
    backgroundColor: colors.primaryFixed,
    alignItems: "center",
    justifyContent: "center",
  },
  title: {
    ...typography.styles.sectionTitle,
    fontWeight: typography.fontWeight.extrabold,
  },
  body: {
    fontSize: typography.fontSize.sm,
    color: colors.textSecondary,
    lineHeight: 20,
  },
  detailBox: {
    backgroundColor: "#FFFFFF",
    borderRadius: borderRadius.md,
    padding: spacing[3],
    gap: spacing[1],
  },
  detailLine: {
    fontSize: typography.fontSize.xs,
    color: colors.textSecondary,
    lineHeight: 16,
  },
  checkboxWrap: {
    marginTop: spacing[1],
  },
  actions: {
    flexDirection: "row",
    gap: spacing[3],
    marginTop: spacing[2],
  },
  cancelBtn: {
    flex: 1,
    height: layout.buttonHeightMd,
    backgroundColor: colors.surfaceContainerHigh,
    borderRadius: borderRadius.lg,
    alignItems: "center",
    justifyContent: "center",
  },
  cancelBtnText: {
    ...typography.styles.buttonLabelSm,
    color: colors.onSurfaceVariant,
  },
  agreeBtn: {
    flex: 1,
    height: layout.buttonHeightMd,
    backgroundColor: colors.primary,
    borderRadius: borderRadius.lg,
    alignItems: "center",
    justifyContent: "center",
    ...shadows.primaryButton,
  },
  agreeBtnDisabled: {
    backgroundColor: colors.secondaryContainer,
    shadowOpacity: 0,
    elevation: 0,
  },
  agreeBtnText: {
    ...typography.styles.buttonLabelSm,
    color: colors.onPrimary,
  },
});

// ---------------------------------------------------------------------------
// Add document modal
// ---------------------------------------------------------------------------

interface AddDocModalProps {
  visible: boolean;
  onClose: () => void;
  /** `file`: the picked file, stored encrypted by lib/vault.ts storeVaultFile (the one vault write path). */
  onAdd: (entry: VaultEntry, file: { sourceUri: string; extension: string; mimeType: string | null } | null) => Promise<void>;
}

type PickedFile = {
  uri: string;
  mimeType: string | null;
  name: string;
  extension: string;
};

function AddDocModal({ visible, onClose, onAdd }: AddDocModalProps) {
  const { user } = useAuthStore();
  const hasHealthInsuranceConsent = useConsentStore((s) => s.hasHealthInsuranceConsent());
  const agreeHealthInsurance = useConsentStore((s) => s.agreeHealthInsurance);
  const [selectedType, setSelectedType] = useState<DocumentType>("resident_register");
  const [issuedDate, setIssuedDate] = useState<string>(() => {
    const d = new Date();
    return d.toISOString().split("T")[0];
  });
  const [pickedFile, setPickedFile] = useState<PickedFile | null>(null);
  const [isSaving, setIsSaving] = useState(false);
  /** Raw camera URI waiting for scan-preview processing */
  const [scanRawUri, setScanRawUri] = useState<string | null>(null);
  /** B: 건강보험 민감정보 별도동의 모달 표시 여부 */
  const [showHealthConsentModal, setShowHealthConsentModal] = useState(false);
  /** C: "주민등록번호 뒷자리가 가려진 서류입니다" 업로드 직전 확인 체크박스 */
  const [maskingConfirmed, setMaskingConfirmed] = useState(false);
  const slideAnim = useRef(new Animated.Value(500)).current;

  /** C: this document type carries RRN risk AND a file is actually attached */
  const requiresMaskingConfirm = !!pickedFile && !!RRN_MASKING_INFO[selectedType];

  // The masking guidance card + confirmation checkbox add variable-height
  // content to this sheet, so it needs to scroll instead of relying on
  // intrinsic sizing (which could push the save button off-screen on
  // smaller devices).
  const { height: windowHeight } = useWindowDimensions();
  const sheetMaxHeight = windowHeight * 0.86;

  useEffect(() => {
    if (visible) {
      Animated.spring(slideAnim, {
        toValue: 0,
        useNativeDriver: true,
        tension: 65,
        friction: 11,
      }).start();
    } else {
      slideAnim.setValue(500);
    }
  }, [visible, slideAnim]);

  async function handleCamera() {
    const { status } = await ImagePicker.requestCameraPermissionsAsync();
    if (status !== "granted") {
      Alert.alert("카메라 권한 필요", "카메라 접근 권한을 허용해주세요.");
      return;
    }
    const result = await ImagePicker.launchCameraAsync({
      mediaTypes: "images",
      // Keep original quality; expo-image-manipulator will compress to 80%
      quality: 1,
      allowsEditing: false,
      exif: false,
    });
    if (!result.canceled && result.assets.length > 0) {
      const asset = result.assets[0];
      // Open scan-preview modal instead of immediately setting pickedFile
      setScanRawUri(asset.uri);
    }
  }

  function handleScanConfirm(processedUri: string) {
    setScanRawUri(null);
    setMaskingConfirmed(false);
    setPickedFile({
      uri: processedUri,
      mimeType: "image/jpeg",
      name: DOC_TYPE_CONFIG[selectedType].label,
      extension: "jpg",
    });
  }

  // iOS refuses to present the camera while the preview is still sliding away,
  // so it reopens from the preview's onDismiss; other platforms reopen now.
  const cameraAfterScan = useRef(false);
  function handleScanRetake() {
    setScanRawUri(null);
    if (Platform.OS === "ios") cameraAfterScan.current = true;
    else handleCamera();
  }
  function handleScanDismiss() {
    if (!cameraAfterScan.current) return;
    cameraAfterScan.current = false;
    handleCamera();
  }

  async function handleGallery() {
    const { status } = await ImagePicker.requestMediaLibraryPermissionsAsync();
    if (status !== "granted") {
      Alert.alert("사진 라이브러리 권한 필요", "사진 접근 권한을 허용해주세요.");
      return;
    }
    const result = await ImagePicker.launchImageLibraryAsync({
      mediaTypes: "images",
      quality: 0.85,
      allowsEditing: false,
    });
    if (!result.canceled && result.assets.length > 0) {
      const asset = result.assets[0];
      const ext = asset.uri.split(".").pop() ?? "jpg";
      setMaskingConfirmed(false);
      setPickedFile({
        uri: asset.uri,
        mimeType: asset.mimeType ?? "image/jpeg",
        name: DOC_TYPE_CONFIG[selectedType].label,
        extension: ext,
      });
    }
  }

  async function handleFilePicker() {
    const result = await DocumentPicker.getDocumentAsync({
      type: ["application/pdf", "image/*"],
      copyToCacheDirectory: true,
    });
    if (!result.canceled && result.assets.length > 0) {
      const asset = result.assets[0];
      const ext = (asset.name ?? "file").split(".").pop() ?? "pdf";
      setMaskingConfirmed(false);
      setPickedFile({
        uri: asset.uri,
        mimeType: asset.mimeType ?? "application/pdf",
        name: asset.name ?? DOC_TYPE_CONFIG[selectedType].label,
        extension: ext,
      });
    }
  }

  function showAddOptions() {
    if (Platform.OS === "ios") {
      ActionSheetIOS.showActionSheetWithOptions(
        {
          options: ["취소", "카메라 촬영", "갤러리에서 선택", "파일(PDF) 선택"],
          cancelButtonIndex: 0,
        },
        (index) => {
          if (index === 1) handleCamera();
          else if (index === 2) handleGallery();
          else if (index === 3) handleFilePicker();
        }
      );
    } else {
      // Android 대화상자는 버튼을 3개까지만 보여 준다. 취소는 바깥 탭·뒤로 가기로 한다.
      Alert.alert("서류 추가 방법", "바깥을 누르면 취소돼요.", [
        { text: "카메라 촬영", onPress: handleCamera },
        { text: "갤러리에서 선택", onPress: handleGallery },
        { text: "파일(PDF) 선택", onPress: handleFilePicker },
      ], { cancelable: true });
    }
  }

  /**
   * B: 건강보험 자격득실확인서를 "업로드하려 할 때만" 민감정보 별도동의를 요구한다.
   * 이미 동의했거나 다른 서류 종류라면 곧바로 실제 파일 선택 액션시트를 연다.
   * 거부해도 다른 서류 종류의 보관 기능은 전혀 영향받지 않는다 — 끼워팔기 금지.
   */
  function handleAttachPress() {
    if (selectedType === "health_insurance" && !hasHealthInsuranceConsent) {
      setShowHealthConsentModal(true);
      return;
    }
    showAddOptions();
  }

  // iOS는 모달이 닫히는 중에 띄운 시트를 버리므로 동의 모달이 닫힌 뒤(onDismiss) 선택지를 연다
  const optionsAfterConsent = useRef(false);
  async function handleHealthConsentAgree() {
    await agreeHealthInsurance();
    setShowHealthConsentModal(false);
    if (Platform.OS === "ios") optionsAfterConsent.current = true;
    else showAddOptions();
  }
  function handleHealthConsentDismiss() {
    if (!optionsAfterConsent.current) return;
    optionsAfterConsent.current = false;
    showAddOptions();
  }

  async function handleSave() {
    // C: RRN 마스킹 확인 체크박스 미체크 시 업로드(파일 저장) 불가
    if (requiresMaskingConfirm && !maskingConfirmed) {
      notify(
        "확인이 필요해요",
        "주민등록번호 뒷자리가 가려진 서류인지 확인 후 체크박스에 체크해주세요."
      );
      return;
    }

    setIsSaving(true);
    try {
      if (pickedFile && !user?.id) {
        notify(
          "로그인 필요",
          "서류를 안전하게 암호화하려면 로그인이 필요합니다. 로그인 후 다시 시도해주세요."
        );
        setIsSaving(false);
        return;
      }

      await onAdd(
        { type: selectedType, name: DOC_TYPE_CONFIG[selectedType].label, issuedAt: new Date(issuedDate).toISOString() },
        pickedFile ? { sourceUri: pickedFile.uri, extension: pickedFile.extension, mimeType: pickedFile.mimeType } : null,
      );
      // Reset form
      setPickedFile(null);
      setSelectedType("resident_register");
      setIssuedDate(new Date().toISOString().split("T")[0]);
      setMaskingConfirmed(false);
    } catch (err) {
      notify("저장 실패", "서류를 저장하지 못했습니다. 다시 시도해주세요.");
    } finally {
      setIsSaving(false);
    }
  }

  function handleClose() {
    setPickedFile(null);
    setScanRawUri(null);
    setShowHealthConsentModal(false);
    setMaskingConfirmed(false);
    onClose();
  }

  const saveDisabled = isSaving || (requiresMaskingConfirm && !maskingConfirmed);

  return (
    <>
      <Modal
        visible={visible}
        transparent
        animationType="fade"
        onRequestClose={handleClose}
      >
      <Pressable style={modalStyles.overlay} onPress={handleClose}>
        <Animated.View
          style={[
            modalStyles.sheet,
            { maxHeight: sheetMaxHeight },
            { transform: [{ translateY: slideAnim }] },
          ]}
        >
          {/* Drag handle */}
          <Pressable
            onPress={(e) => e.stopPropagation()}
            style={modalStyles.sheetInner}
          >
            <View style={modalStyles.handle} />

            <Text style={modalStyles.sheetTitle}>서류 추가</Text>

            <ScrollView
              style={modalStyles.fieldsScroll}
              contentContainerStyle={modalStyles.fieldsScrollContent}
              showsVerticalScrollIndicator={false}
              keyboardShouldPersistTaps="handled"
            >
              {/* Document type selector */}
              <Text style={modalStyles.fieldLabel}>서류 종류</Text>
              <ScrollView
                horizontal
                showsHorizontalScrollIndicator={false}
                contentContainerStyle={modalStyles.typeScrollContent}
                style={modalStyles.typeScroll}
              >
                {DOCUMENT_TYPE_OPTIONS.map((type) => {
                  const cfg = DOC_TYPE_CONFIG[type];
                  const isSelected = selectedType === type;
                  return (
                    <Tappable feedback="opacity"
                      key={type}
                      style={[
                        modalStyles.typeChip,
                        isSelected && modalStyles.typeChipSelected,
                        { borderColor: isSelected ? cfg.accentColor : "transparent" },
                      ]}
                      onPress={() => setSelectedType(type)}
                      accessibilityRole="radio"
                      accessibilityState={{ selected: isSelected }}
                      accessibilityLabel={cfg.label}
                    >
                      <Text
                        style={[
                          modalStyles.typeChipText,
                          isSelected && { color: cfg.accentColor },
                        ]}
                      >
                        {cfg.shortLabel}
                      </Text>
                    </Tappable>
                  );
                })}
              </ScrollView>

              {/* C: 주민번호 마스킹 발급 가이드 — 서류 종류 선택 직후, 실제 발급받으러
                  가기 전에 보이도록 배치 */}
              <MaskingGuidanceCard type={selectedType} />

              {/* Issued date — simple text representation (date picker is platform-specific;
                  a full DateTimePicker is a future enhancement) */}
              <Text style={modalStyles.fieldLabel}>발급일</Text>
              <View style={modalStyles.dateDisplay}>
                <Text style={modalStyles.dateDisplayText}>
                  {issuedDate}
                </Text>
              </View>

              {/* File picker area */}
              <Text style={modalStyles.fieldLabel}>파일 첨부 (선택)</Text>
              {Platform.OS === "web" ? (
                // 웹은 서류 목록만 관리한다. 파일은 기기 안에서만 암호화해 보관하므로 앱으로 안내한다.
                <View style={modalStyles.webFileNotice}>
                  <Text style={modalStyles.webFileNoticeText}>파일은 마지미 앱에서 보관할 수 있어요</Text>
                  <Text
                    accessibilityRole="link"
                    style={modalStyles.webFileNoticeLink}
                    onPress={() => void Linking.openURL(APP_INSTALL_URL).catch(() => {})}
                  >
                    마지미 앱 알아보기
                  </Text>
                </View>
              ) : (
              <Tappable feedback="opacity"
                style={[
                  modalStyles.filePicker,
                  pickedFile && modalStyles.filePickerFilled,
                ]}
                onPress={handleAttachPress}
                accessibilityRole="button"
                accessibilityLabel={pickedFile ? "첨부 파일 변경" : "파일 또는 사진 추가"}
              >
                <FigmaIcon name={pickedFile ? "check" : "close"} size={12} color={colors.primary} rotate={pickedFile ? undefined : 45} />
                <Text style={modalStyles.filePickerText}>
                  {pickedFile
                    ? pickedFile.name
                    : "카메라 촬영 · 갤러리 · PDF 선택"}
                </Text>
              </Tappable>
              )}

              {/* C: 업로드 직전 확인 체크박스 — 파일이 첨부되고, 주민번호 위험이 있는
                  서류 종류일 때만 노출. 미체크 시 handleSave에서 저장을 막는다. */}
              {requiresMaskingConfirm && (
                <View style={modalStyles.maskingCheckWrap}>
                  <AgreementCheckbox
                    checked={maskingConfirmed}
                    onToggle={() => setMaskingConfirmed((v) => !v)}
                    label="주민등록번호 뒷자리가 가려진 서류입니다"
                    requirement="required"
                  />
                </View>
              )}
            </ScrollView>

            {/* Action buttons */}
            <View style={modalStyles.actions}>
              <Tappable feedback="opacity"
                style={modalStyles.cancelBtn}
                onPress={handleClose}
                accessibilityRole="button"
                accessibilityLabel="취소"
              >
                <Text style={modalStyles.cancelBtnText}>취소</Text>
              </Tappable>
              <Tappable feedback="opacity"
                style={[modalStyles.saveBtn, saveDisabled && modalStyles.saveBtnDisabled]}
                onPress={handleSave}
                disabled={saveDisabled}
                accessibilityRole="button"
                accessibilityLabel="서류 저장하기"
                accessibilityState={{ disabled: saveDisabled }}
              >
                {isSaving ? (
                  <ActivityIndicator size="small" color={colors.onPrimary} />
                ) : (
                  <Text style={modalStyles.saveBtnText}>저장하기</Text>
                )}
              </Tappable>
            </View>
          </Pressable>
        </Animated.View>
      </Pressable>
      {/* iOS presents a modal from its parent's view controller, and the root one
          is already showing this sheet, so these must live inside this <Modal>.
          They sit outside the overlay Pressable so their taps never close the sheet. */}
      <ScanPreviewModal
        rawUri={scanRawUri}
        onRetake={handleScanRetake}
        onConfirm={handleScanConfirm}
        onDismiss={handleScanDismiss}
      />
      <HealthInsuranceConsentModal
        visible={showHealthConsentModal}
        onCancel={() => setShowHealthConsentModal(false)}
        onAgree={handleHealthConsentAgree}
        onDismiss={handleHealthConsentDismiss}
      />
    </Modal>
    </>
  );
}

const modalStyles = StyleSheet.create({
  webFileNotice: {
    backgroundColor: "#FFFFFF",
    borderRadius: borderRadius.lg,
    paddingHorizontal: spacing[4],
    paddingVertical: spacing[3],
    gap: spacing[1],
  },
  webFileNoticeText: {
    ...typography.styles.bodySm,
    color: colors.textSecondary,
  },
  webFileNoticeLink: {
    ...typography.styles.bodySm,
    fontWeight: "700",
    color: colors.primary,
    textDecorationLine: "underline",
    alignSelf: "flex-start",
    minHeight: layout.touchTargetMin,
    paddingVertical: spacing[3],
  },
  overlay: {
    flex: 1,
    backgroundColor: colors.overlay,
    justifyContent: "flex-end",
  },
  sheet: {
    backgroundColor: colors.surfaceContainerLowest,
    borderTopLeftRadius: borderRadius["2xl"],
    borderTopRightRadius: borderRadius["2xl"],
    ...shadows.floating,
  },
  // The sheet sizes to its content and stops at sheetMaxHeight. `flex: 1` here
  // (and on the ScrollView) collapsed it to zero height on iOS: the sheet has
  // only a maxHeight, so there is no height for flex: 1 to fill. Grow 0 and
  // shrink 1 let the fields scroll once the content is taller than the cap.
  sheetInner: {
    flexGrow: 0,
    flexShrink: 1,
    padding: spacing[6],
    paddingBottom: spacing[8],
    gap: spacing[3],
  },
  fieldsScroll: {
    flexGrow: 0,
    flexShrink: 1,
  },
  fieldsScrollContent: {
    gap: spacing[3],
    paddingBottom: spacing[2],
  },
  maskingCheckWrap: {
    backgroundColor: "#FFFFFF",
    borderRadius: borderRadius.lg,
    paddingHorizontal: spacing[3],
  },
  handle: {
    width: 40,
    height: 4,
    backgroundColor: colors.outlineVariant,
    borderRadius: borderRadius.full,
    alignSelf: "center",
    marginBottom: spacing[2],
  },
  sheetTitle: {
    ...typography.styles.sectionTitle,
    fontWeight: typography.fontWeight.extrabold,
    marginBottom: spacing[1],
  },
  fieldLabel: {
    ...typography.styles.bodySm,
    fontWeight: typography.fontWeight.semibold,
    color: colors.textSecondary,
    marginTop: spacing[2],
  },
  typeScroll: {
    marginHorizontal: -spacing[1],
  },
  typeScrollContent: {
    gap: spacing[2],
    paddingHorizontal: spacing[1],
    paddingVertical: spacing[1],
  },
  typeChip: {
    paddingHorizontal: spacing[4],
    paddingVertical: spacing[2],
    borderRadius: borderRadius.full,
    backgroundColor: colors.surfaceContainerHigh,
    borderWidth: 1,
    minHeight: layout.touchTargetMin - 8,
    alignItems: "center",
    justifyContent: "center",
  },
  typeChipSelected: {
    backgroundColor: colors.primaryFixed,
  },
  typeChipText: {
    fontSize: typography.fontSize.sm, lineHeight: 20,
    fontWeight: typography.fontWeight.semibold,
    color: colors.onSurfaceVariant,
  },
  dateDisplay: {
    backgroundColor: "#FFFFFF",
    borderRadius: borderRadius.lg,
    paddingHorizontal: spacing[4],
    paddingVertical: spacing[3.5],
    minHeight: layout.inputHeight,
    justifyContent: "center",
  },
  dateDisplayText: {
    ...typography.styles.inputText,
    color: colors.onSurface,
  },
  filePicker: {
    flexDirection: "row",
    alignItems: "center",
    gap: spacing[3],
    backgroundColor: "#FFFFFF",
    borderRadius: borderRadius.lg,
    paddingHorizontal: spacing[4],
    paddingVertical: spacing[4],
    minHeight: layout.touchTargetMin + spacing[2],
  },
  filePickerFilled: {
    backgroundColor: colors.primaryFixed,
  },
  filePickerText: {
    flex: 1,
    ...typography.styles.bodySm,
    color: colors.textSecondary,
  },
  actions: {
    flexDirection: "row",
    gap: spacing[3],
    marginTop: spacing[4],
  },
  cancelBtn: {
    flex: 1,
    height: layout.buttonHeightMd,
    backgroundColor: colors.surfaceContainerHigh,
    borderRadius: borderRadius.lg,
    alignItems: "center",
    justifyContent: "center",
  },
  cancelBtnText: {
    ...typography.styles.buttonLabelSm,
    color: colors.onSurfaceVariant,
  },
  saveBtn: {
    flex: 2,
    height: layout.buttonHeightMd,
    backgroundColor: colors.primary,
    borderRadius: borderRadius.lg,
    alignItems: "center",
    justifyContent: "center",
    ...shadows.primaryButton,
  },
  saveBtnDisabled: {
    backgroundColor: colors.secondaryContainer,
    shadowOpacity: 0,
    elevation: 0,
  },
  saveBtnText: {
    ...typography.styles.buttonLabel,
    color: colors.onPrimary,
  },
});

// ---------------------------------------------------------------------------
// Empty state
// ---------------------------------------------------------------------------

function EmptyVault({ onAdd }: { onAdd: () => void }) {
  return (
    <View style={emptyStyles.root}>
      <Image source={require("../assets/figma/search-empty.png")} style={emptyStyles.art} resizeMode="contain" accessibilityIgnoresInvertColors />
      <Text style={emptyStyles.title}>보관 중인 서류가 없어요</Text>
      <Text style={emptyStyles.body}>
        자주 쓰는 서류를 미리 추가해두면{"\n"}혜택 신청 시 바로 활용할 수 있어요
      </Text>
      <Tappable feedback="opacity"
        style={emptyStyles.btn}
        onPress={onAdd}
        accessibilityRole="button"
        accessibilityLabel="첫 서류 추가하기"
      >
        <Text style={emptyStyles.btnText}>첫 서류 추가하기</Text>
      </Tappable>
    </View>
  );
}

// 서류 검색 empty state 34061:3119: greyscale illustration at 40%, 16/24 title, 13/20 body.
const emptyStyles = StyleSheet.create({
  root: { alignItems: "center", paddingVertical: spacing[8], gap: spacing[2], paddingHorizontal: spacing[4] },
  art: { width: 183, height: 183, opacity: 0.4 },
  title: { ...fig.text.bold16, color: fig.color.black, textAlign: "center" },
  body: { ...fig.text.body13, color: fig.color.deepGray, textAlign: "center" },
  btn: { marginTop: spacing[2], height: fig.component.button.height, borderRadius: fig.component.button.radius, backgroundColor: fig.color.primary, paddingHorizontal: fig.component.button.paddingHorizontal, alignItems: "center", justifyContent: "center" },
  btnText: { ...fig.text.bold14, color: fig.color.white },
});

// ---------------------------------------------------------------------------
// Main screen
// ---------------------------------------------------------------------------

export default function DocumentVaultScreen({ embedded = false }: { embedded?: boolean }) {
  const documentItems = useDocumentItemsSupport();
  // A restoring member's tap opens the sheet once the session is back; only a guest is asked to log in.
  const member = useMemberAction({ add: { run: () => setShowAddModal(true) } });
 const router=useRouter();const {user}=useAuthStore();const client=useQueryClient();
 const [documents,setDocuments]=useState<StoredDocument[]>([]);const [isLoading,setIsLoading]=useState(true);const [showAddModal,setShowAddModal]=useState(false);const [category,setCategory]=useState("");const [error,setError]=useState("");
 // 34060:4256 header search: filters the saved documents by name on this device.
 const [showSearch,setShowSearch]=useState(false);const [search,setSearch]=useState("");const confirm=useConfirmDialog();
 useEffect(()=>{setDocuments([]);setIsLoading(true);if(!user?.id)setIsLoading(false)},[user?.id]);
 // Re-read on every focus: the detail card and the form helper add files while this tab stays mounted.
 useFocusEffect(useCallback(()=>{let mounted=true;if(!user?.id)return;void loadVaultDocuments(user.id).then(d=>{if(mounted){setDocuments(d);setIsLoading(false)}});return()=>{mounted=false}},[user?.id]));
 // One write path: a picked file is stored by storeVaultFile inside the queue, with the
 // generation taken now, so a save racing an account clear is refused and leaves no file.
 const handleAdd=async(entry:VaultEntry,file:{sourceUri:string;extension:string;mimeType:string|null}|null)=>{if(!user?.id)return;const owner=user.id;const started=vaultGeneration(owner);if(file)await storeVaultFile(owner,{sourceUri:file.sourceUri},{...entry,mimeType:file.mimeType},{extension:file.extension,started});else await addVaultEntry(owner,entry);const next=await loadVaultDocuments(owner);if(useAuthStore.getState().user?.id===owner)setDocuments(next);void client.invalidateQueries({queryKey:["vault-documents",owner]});setShowAddModal(false)};
 const remove=async(doc:StoredDocument)=>{if(!user?.id)return;try{const owner=user.id;const next=await updateVault(owner,async docs=>{const kept=docs.filter(d=>d.id!==doc.id);return{docs:kept,result:kept}});if(useAuthStore.getState().user?.id===owner)setDocuments(next);void client.invalidateQueries({queryKey:["vault-documents",user.id]});if(doc.fileUri)await FileSystem.deleteAsync(doc.fileUri,{idempotent:true})}catch{setError("서류를 삭제하지 못했어요.")}};
 const open=async(doc:StoredDocument)=>{if(!user?.id||!doc.fileUri)return;let uri:string|null=null;try{uri=await decryptVaultFile(user.id,doc);if(await Sharing.isAvailableAsync())await Sharing.shareAsync(uri,{mimeType:doc.mimeType??undefined});else setError("이 기기에서는 파일을 열 수 없어요.")}catch(e){setError(e instanceof VaultFileMissingError?e.message:"서류를 열지 못했어요. 다시 등록해 주세요.")}finally{if(uri)await FileSystem.deleteAsync(uri,{idempotent:true}).catch(()=>{})}};
 const tabs=[["","전체"],["resident_register","주민등록"],["enrollment_cert","재학증명"],["income_proof","소득증빙"],["other","기타"]];const key=search.trim().toLocaleLowerCase();const filtered=documents.filter(d=>(!category||(category==="other"?!["resident_register","enrollment_cert","income_proof"].includes(d.type):d.type===category))&&(!key||d.name.toLocaleLowerCase().includes(key)));
 // Figma 서류 보관함 34059:3468: title header, tabs, count row, document cards 34060:4193.
 return <View style={ui.screen}><ScreenHeader title="서류 보관함" back={!embedded} tabs action={<View style={{flexDirection:"row"}}><Tappable style={ui.iconButton} accessibilityRole="button" accessibilityLabel="서류 검색 열기" accessibilityState={{expanded:showSearch}} onPress={()=>{setShowSearch(!showSearch);if(showSearch)setSearch("")}}><FigmaIcon name="searchMono" size={20} color={fig.color.grayC8}/></Tappable>{embedded&&<NotificationBell/>}</View>}/><View style={ui.tabs}><ScrollView horizontal showsHorizontalScrollIndicator={false} contentContainerStyle={{paddingHorizontal:16}}>{tabs.map(([id,label])=><Tappable key={id} accessibilityRole="tab" accessibilityState={{selected:category===id}} onPress={()=>setCategory(id)} style={[ui.tab,category===id&&ui.tabSelected]}><Text style={[ui.tabText,category===id&&ui.tabTextSelected]}>{label}</Text></Tappable>)}</ScrollView></View>
 {showSearch&&<SearchBar value={search} onChange={setSearch} onSubmit={()=>setSearch(search.trim())} placeholder="서류명을 검색해주세요" label="서류 검색"/>}
 <ScrollView contentContainerStyle={[ui.content,{paddingTop:12}]}><View style={vault.countRow}><Text style={vault.count}>저장된 서류 <Text style={vault.countNumber}>{filtered.length}건</Text></Text><Tappable accessibilityRole="button" accessibilityLabel="서류 추가" style={vault.add} onPress={()=>member.run("add",()=>requireLogin(router,"서류를 보관하려면 로그인해 주세요."))}><FigmaIcon name="close" size={12} color={fig.color.secondary} rotate={45} /><Text style={vault.addText}>서류 추가</Text></Tappable></View>
 {isLoading&&<ActivityIndicator color={fig.color.primary}/>}{!!error&&<Text style={vault.error}>{error}</Text>}
 {filtered.map(doc=>{const cfg=DOC_TYPE_CONFIG[doc.type];const status=computeStatus(doc);const stored=!!doc.fileUri;return <View key={doc.id} style={[vault.card,{backgroundColor:stored?fig.color.grayF7:fig.color.input2}]}><View style={vault.cardRow}><View style={{flex:1,gap:4}}><Text style={vault.name}>{doc.name}</Text>{doc.source?.kind!=="filled_form"&&<Text style={ui.muted}>발급처: {cfg.issuerLabel}</Text>}</View><Tappable onPress={()=>doc.fileUri?void open(doc):cfg.issuerUrl?void Linking.openURL(cfg.issuerUrl):setShowAddModal(true)} style={[vault.action,{backgroundColor:stored?fig.color.disabled:fig.color.blue}]}><Text style={vault.actionText}>{stored?"확인":"연결"}</Text><FigmaIcon name={stored?"check":"link"} size={12} color={stored?fig.color.white:fig.color.primary2}/></Tappable></View>
 {(status!=="valid"||!doc.fileUri)&&<View style={vault.notice}><FigmaIcon name="info" size={12} color={fig.color.skyblue}/><Text style={vault.noticeText}>확인 필요: {!doc.fileUri?"서류 파일을 아직 보관하지 않았어요.":status==="expired"?"등록한 유효기간이 지났어요.":"신청처의 발급일 기준을 확인해 주세요."}</Text></View>}
 <View style={ui.row}><Text style={ui.muted}>발급일 {formatDate(doc.issuedAt)}</Text><Tappable accessibilityRole="button" accessibilityLabel={`${doc.name} 삭제`} onPress={()=>confirm.ask({title:"서류 삭제",message:"이 서류를 보관함에서 삭제하시겠습니까?",confirm:"삭제",onConfirm:()=>void remove(doc)})} style={{marginLeft:"auto",padding:8,marginVertical:-8}}><Text style={ui.muted}>삭제</Text></Tappable></View></View>})}
 {!isLoading&&!filtered.length&&key?<Text style={[ui.muted,{textAlign:"center",paddingVertical:24}]}>검색어에 맞는 서류가 없어요</Text>:null}
 {!isLoading&&!filtered.length&&!key&&<EmptyVault onAdd={()=>member.run("add",()=>requireLogin(router,"서류를 보관하려면 로그인해 주세요."))}/>}
 {/* F6.2: my own checklist entries (names and status only; files stay on this device) */}
 {documentItems===true&&<View style={[vault.card,{paddingHorizontal:0,backgroundColor:fig.color.white}]}><View style={{paddingHorizontal:16,gap:4}}><Text style={vault.name}>준비할 서류 체크리스트</Text></View><MyDocumentItems programId={null} inset={16} onGuest={()=>requireLogin(router,"서류를 직접 추가하려면 로그인해 주세요.")}/></View>}</ScrollView><AddDocModal visible={showAddModal} onClose={()=>setShowAddModal(false)} onAdd={handleAdd}/>{confirm.dialog}{member.toastElement}</View>
}

// 34059:3469: count row 34059:3470, card 34060:4193, button 34060:4202, notice 34060:4205.
const vault = StyleSheet.create({
  countRow: { flexDirection: "row", alignItems: "center", minHeight: 34, paddingVertical: 8, marginBottom: 0 },
  count: { ...fig.text.bold12, color: fig.color.black }, countNumber: { color: fig.color.primary },
  add: { marginLeft: "auto", minHeight: 44, flexDirection: "row", alignItems: "center", gap: 4, justifyContent: "center", paddingHorizontal: 4, marginVertical: -12 }, addText: { ...fig.text.bold12, color: fig.color.secondary },
  error: { ...fig.text.body12, color: fig.color.orange, marginBottom: 16 },
  card: { borderWidth: 1, borderColor: fig.color.grayF1, borderRadius: fig.component.card.radius, padding: 16, gap: 12, marginTop: 16 },
  cardRow: { flexDirection: "row", alignItems: "center", gap: 12 },
  name: { ...fig.text.bold14, color: fig.color.black },
  action: { flexDirection: "row", alignItems: "center", gap: 4, height: 34, paddingHorizontal: 16, borderRadius: fig.component.badge.radius, boxShadow: `${fig.shadow.badge}, ${fig.shadow.badgeInner}` },
  actionText: { ...fig.text.bold12, color: fig.color.white },
  notice: { flexDirection: "row", alignItems: "center", gap: 4, paddingVertical: 8, paddingLeft: 16, paddingRight: 12, borderRadius: fig.component.infoBox.radius, borderWidth: 1, borderColor: fig.color.secondary4, backgroundColor: fig.color.secondary4 },
  noticeText: { ...fig.text.body12, color: fig.color.gray66, flex: 1 },
});

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------

const styles = StyleSheet.create({
  root: {
    flex: 1,
    backgroundColor: "#FFFFFF",
  },

  // Header
  header: {
    backgroundColor: colors.surfaceContainerLowest,
    zIndex: 50,
    ...Platform.select({
      ios: {
        shadowColor: colors.secondaryFixedDim,
        shadowOffset: { width: 0, height: 2 },
        shadowOpacity: 0.08,
        shadowRadius: 8,
      },
      android: { elevation: 4 },
    }),
  },
  headerInner: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    paddingHorizontal: spacing[5],
    paddingBottom: spacing[4],
  },
  headerBackBtn: {
    minWidth: layout.touchTargetMin,
    minHeight: layout.touchTargetMin,
    alignItems: "center",
    justifyContent: "center",
  },
  headerTitleWrap: {
    flexDirection: "row",
    alignItems: "center",
    gap: spacing[2],
  },
  headerTitle: {
    fontSize: typography.fontSize.md, lineHeight: 24,
    fontWeight: typography.fontWeight.bold,
    color: colors.onSurface,
  },
  headerBadge: {
    backgroundColor: "#F72B2B0A",
    borderRadius: borderRadius.full,
    paddingHorizontal: spacing[2],
    paddingVertical: 2,
    minWidth: 20,
    alignItems: "center",
  },
  headerBadgeText: {
    fontSize: typography.fontSize.xs, lineHeight: 16,
    fontWeight: typography.fontWeight.semibold,
    color: "#F75B2B",
  },

  scroll: {
    flex: 1,
  },

  loadingCenter: {
    flex: 1,
    alignItems: "center",
    justifyContent: "center",
  },

  // Renewal banner
  renewalBanner: {
    backgroundColor: "#F72B2B0A",
    marginHorizontal: spacing[4],
    marginTop: spacing[4],
    borderRadius: borderRadius.lg,
    paddingVertical: spacing[3],
    paddingHorizontal: spacing[4],
  },
  renewalBannerText: {
    fontSize: typography.fontSize.sm, lineHeight: 20,
    fontWeight: typography.fontWeight.semibold,
    color: "#F75B2B",
  },

  // Statistics strip
  statsStrip: {
    flexDirection: "row",
    backgroundColor: colors.surfaceContainerLowest,
    marginHorizontal: spacing[4],
    marginTop: spacing[4],
    borderRadius: borderRadius.xl,
    paddingVertical: spacing[5],
    ...shadows.card,
  },
  statCell: {
    flex: 1,
    alignItems: "center",
    gap: spacing[1],
  },
  statDivider: {
    width: 1,
    height: 32,
    backgroundColor: colors.surfaceContainerHigh,
    alignSelf: "center",
  },
  statNumber: {
    fontSize: typography.fontSize["2xl"], lineHeight: 36,
    fontWeight: typography.fontWeight.extrabold,
    color: colors.onSurface,
    letterSpacing: typography.letterSpacing.tight,
  },
  statNumberWarning: {
    color: "#F75B2B",
  },
  statLabel: {
    ...typography.styles.caption,
    color: colors.textMuted,
  },

  // Section headers
  sectionHeader: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    paddingHorizontal: spacing[5],
    paddingTop: spacing[6],
    paddingBottom: spacing[2],
  },
  sectionTitle: {
    ...typography.styles.sectionTitle,
  },
  addBtn: {
    backgroundColor: colors.primaryFixed,
    borderRadius: borderRadius.full,
    paddingHorizontal: spacing[4],
    paddingVertical: spacing[1.5],
    minHeight: 32,
    alignItems: "center",
    justifyContent: "center",
  },
  addBtnText: {
    fontSize: typography.fontSize.sm, lineHeight: 20,
    fontWeight: typography.fontWeight.semibold,
    color: colors.onPrimaryFixedVariant,
  },

  // Document list
  docList: {
    paddingHorizontal: spacing[4],
    gap: spacing[3],
  },

  // Section strip (tonal divider, No-Line Rule)
  sectionStrip: {
    height: spacing[3],
    backgroundColor: "#FFFFFF",
    marginTop: spacing[6],
  },

  // FAB
  fab: {
    position: "absolute",
    left: spacing[5],
    right: spacing[5],
    alignItems: "center",
  },
  fabBtn: {
    backgroundColor: colors.primary,
    borderRadius: borderRadius.full,
    paddingHorizontal: spacing[8],
    height: layout.buttonHeightMd,
    alignItems: "center",
    justifyContent: "center",
    ...shadows.primaryButton,
  },
  fabBtnText: {
    ...typography.styles.buttonLabel,
    color: colors.onPrimary,
  },
});
