import React, { useState } from "react";
import { StyleSheet, TextInput, View } from "react-native";
import { FigmaIcon } from "./FigmaUI";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import Text from "./Text";
import Tappable from "./Tappable";
import { api, ApiRequestError, validDocumentItemName, type DocumentItem } from "@/lib/api";
import { useAuthStore } from "@/store/auth";
import { useDocumentItemsSupport } from "@/lib/catalogue";
import { useMemberAction } from "./MemberAction";
import { colors } from "@/constants/theme";

// 서류 목록에 내가 직접 추가하는 항목 (v1.0.0 피드백 F6.2). 이름과 완료 여부만 서버에 남고 파일은 기기를 떠나지 않는다.

export const ADD_DOCUMENT_LABEL = "직접 추가";

/** `programId` null = items not tied to a programme (서류 보관함 checklist). `onGuest`: a guest tapped 직접 추가. */
export default function MyDocumentItems({ programId, onGuest, inset = 14 }: { programId: string | null; onGuest: () => void; inset?: number }) {
  const user = useAuthStore(s => s.user);
  const client = useQueryClient();
  const key = ["document-items", user?.id, programId];
  const supported = useDocumentItemsSupport();
  const items = useQuery({ queryKey: key, queryFn: () => api.getDocumentItems(programId ?? undefined), enabled: !!user && supported === true, retry: (count, error) => !(error instanceof ApiRequestError && error.status === 404) && count < 2 });
  const [adding, setAdding] = useState(false);
  const [name, setName] = useState("");
  const [error, setError] = useState("");
  // A restoring member's tap runs once the session is back; an unconfirmed one gets the message inline.
  const member = useMemberAction({ add: { run: () => setAdding(true) } }, { onUnconfirmed: setError });
  const refresh = () => client.invalidateQueries({ queryKey: ["document-items", user?.id] });
  const add = useMutation({ mutationFn: (value: string) => api.addDocumentItem(value, programId), onSuccess: () => { setName(""); setAdding(false); setError(""); return refresh(); }, onError: (e: Error) => setError(e.message) });
  const update = useMutation({ mutationFn: (item: DocumentItem) => api.updateDocumentItem(item.id, { status: item.status === "done" ? "todo" : "done" }), onSuccess: refresh, onError: (e: Error) => setError(e.message) });
  const remove = useMutation({ mutationFn: (id: string) => api.deleteDocumentItem(id), onSuccess: refresh, onError: (e: Error) => setError(e.message) });
  // Until the route is known to exist (older servers have none), show nothing:
  // not to members, and not as a login prompt to guests.
  if (supported !== true) return null;
  if (items.error instanceof ApiRequestError && items.error.status === 404) return null;
  const visible = (items.data?.items ?? []).filter(item => programId === null ? item.program_id === null : item.program_id === programId);
  const submit = () => {
    if (!validDocumentItemName(name)) { setError("서류 이름을 1~60자로 입력해 주세요."); return; }
    add.mutate(name.trim());
  };
  return <View style={{ paddingHorizontal: inset }}>
    {visible.map(item => <View key={item.id} style={s.row}>
      <Tappable accessibilityRole="checkbox" accessibilityState={{ checked: item.status === "done" }} accessibilityLabel={`${item.name} 준비 완료`} loading={update.isPending && update.variables?.id === item.id} onPress={() => update.mutate(item)} style={s.check}>
        <FigmaIcon name="checkCircle" size={20} color={item.status === "done" ? colors.primary : "#8894A1"} />
      </Tappable>
      <View style={{ flex: 1 }}><Text style={[s.name, item.status === "done" && s.done]}>{item.name}</Text><Text style={s.mine}>직접 추가한 서류</Text></View>
      <Tappable accessibilityRole="button" accessibilityLabel={`${item.name} 삭제`} loading={remove.isPending && remove.variables === item.id} onPress={() => remove.mutate(item.id)} style={s.remove}><Text style={s.removeText}>삭제</Text></Tappable>
    </View>)}
    {adding ? <View style={s.form}>
      <TextInput autoFocus value={name} onChangeText={value => { setName(value); setError(""); }} placeholder="서류 이름" placeholderTextColor="#8894A1" maxLength={60} returnKeyType="done" onSubmitEditing={submit} style={s.input} accessibilityLabel="추가할 서류 이름" />
      <Tappable accessibilityRole="button" onPress={submit} loading={add.isPending} spinnerColor="#fff" style={s.save} feedback="opacity"><Text style={s.saveText}>추가</Text></Tappable>
      <Tappable accessibilityRole="button" onPress={() => { setAdding(false); setName(""); setError(""); }} style={s.cancel}><Text style={s.removeText}>취소</Text></Tappable>
    </View> : <Tappable accessibilityRole="button" accessibilityLabel={`필요 서류 ${ADD_DOCUMENT_LABEL}`} loading={member.waiting === "add"} onPress={() => member.run("add", onGuest)} style={s.addButton}>
      <FigmaIcon name="close" size={12} color="#239969" rotate={45} /><Text style={s.addText}>{ADD_DOCUMENT_LABEL}</Text>
    </Tappable>}
    {!!error && <Text accessibilityRole="alert" style={s.error}>{error}</Text>}
  </View>;
}

const s = StyleSheet.create({
  row: { flexDirection: "row", alignItems: "center", gap: 6, paddingVertical: 8, borderBottomWidth: 1, borderColor: "#F1F1F1" },
  check: { width: 44, height: 44, alignItems: "center", justifyContent: "center", borderRadius: 22, marginLeft: -12 },
  name: { fontSize: 13, lineHeight: 20, color: "#070428" },
  done: { color: "#8894A1", textDecorationLine: "line-through" },
  mine: { fontSize: 11, lineHeight: 16, color: "#8894A1" },
  remove: { minHeight: 44, minWidth: 44, paddingHorizontal: 8, alignItems: "center", justifyContent: "center", borderRadius: 8 },
  removeText: { fontSize: 12, lineHeight: 18, color: "#8894A1" },
  form: { flexDirection: "row", alignItems: "center", gap: 6, marginTop: 10 },
  input: { flex: 1, height: 44, borderWidth: 1, borderColor: "#2A8F6652", borderRadius: 10, paddingHorizontal: 12, fontSize: 13, lineHeight: 20, fontFamily: "Pretendard", color: "#070428" },
  save: { height: 44, paddingHorizontal: 16, borderRadius: 10, backgroundColor: colors.primary, alignItems: "center", justifyContent: "center" },
  saveText: { fontSize: 13, lineHeight: 20, fontWeight: "600", color: "#fff" },
  cancel: { height: 44, paddingHorizontal: 8, alignItems: "center", justifyContent: "center", borderRadius: 8 },
  addButton: { flexDirection: "row", alignItems: "center", justifyContent: "center", gap: 4, minHeight: 44, marginTop: 10, borderRadius: 10, borderWidth: 1, borderColor: "#2A8F6652", backgroundColor: "#2A8F660A" },
  addText: { fontSize: 13, lineHeight: 20, fontWeight: "600", color: "#239969" },
  error: { fontSize: 12, lineHeight: 18, color: "#F75B2B", marginTop: 6 },
});
