import React,{useState} from "react";
import { Pressable,ScrollView,TextInput,View } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useMutation,useQuery,useQueryClient } from "@tanstack/react-query";
import ApplicationJourneyCard from "@/components/ApplicationJourneyCard";
import Text from "@/components/Text";
import { BulletRow,EmptyState,ScreenHeader,ui } from "@/components/FigmaUI";
import { api,type ApplicationStatus } from "@/lib/api";
import { invalidateAfterWrite } from "@/lib/writeEffects";
import { fig } from "@/constants/theme";
import { useAuthStore } from "@/store/auth";
import { applicationMethodText } from "@/constants/displayLabels";
/** Labels for the user's own 7-step record. The headline status is the server's `display_label`. */
const labels:Record<ApplicationStatus,string>={interested:"관심",planning:"신청 예정",applying:"신청 준비 중",applied:"신청 완료 · 직접 기록",waiting:"결과 대기",received:"수혜 완료",abandoned:"진행 중단 · 직접 기록"};
export default function Application(){const {id}=useLocalSearchParams<{id:string}>();const router=useRouter();const user=useAuthStore(s=>s.user);const client=useQueryClient();const [memo,setMemo]=useState<string|null>(null);const query=useQuery({queryKey:["application",user?.id,id],queryFn:()=>api.getApplicationStatus(id),enabled:!!id&&!!user});const mutation=useMutation({mutationFn:(status:ApplicationStatus)=>api.updateApplicationStatus(id,status,memo??query.data?.memo??""),onSuccess:()=>invalidateAfterWrite(client,'record_application_status',{userId:user?.id,programId:id})});const item=query.data;const p=item?.snapshot?.program;
return <View style={ui.screen}><ScreenHeader title="신청 이력" back/><ScrollView contentContainerStyle={[ui.content,{paddingTop:20}]}>
{item?<><Text style={ui.sectionTitle}>{item.program_title}</Text><Text style={[ui.muted,{marginTop:8,marginBottom:24}]}>{item.display_label} · 마감 후에도 이력은 유지돼요</Text>
{p&&<View style={ui.benefitCard}><Text style={ui.cardTitle}>보관된 혜택 내용</Text><Text style={ui.cardSummary}>{p.summary}</Text>{p.conditions&&<Text style={ui.cardSummary}>{p.conditions}</Text>}{!!applicationMethodText(p.application_method)&&<Text style={ui.cardSummary}>{applicationMethodText(p.application_method)}</Text>}{p.required_documents?.map((d,i)=><BulletRow key={i} style={ui.muted} color={fig.color.deepGray}>{d.document_name}{d.is_required?" (필수)":""}</BulletRow>)}<Text style={[ui.muted,{marginTop:12}]}>{(item.snapshot?.capture_reason==="submission"||item.snapshot?.capture_reason==="handoff_report")?"신청 준비·완료 당시 보관한 내용이에요.":item.snapshot?.capture_reason==="migration_current"?"이력 보관 기능이 추가된 시점의 내용이에요.":"처음 저장한 시점의 내용이에요."}</Text></View>}
{item.journey&&<ApplicationJourneyCard key={item.journey.id} journey={item.journey}/>}
{!["applied","waiting","received","abandoned"].includes(item.current_status)&&<Pressable accessibilityRole="button" onPress={()=>router.push(`/apply-assistant?programId=${id}`)} style={[ui.primaryButton,{marginVertical:16}]}><Text style={ui.primaryButtonText}>신청 준비 이어가기</Text></Pressable>}
<Text style={ui.cardTitle}>내가 기록한 진행 상태</Text><View style={{flexDirection:"row",flexWrap:"wrap",gap:8,marginVertical:16}}>{(Object.keys(labels) as ApplicationStatus[]).map(status=><Pressable key={status} onPress={()=>mutation.mutate(status)} disabled={mutation.isPending} accessibilityRole="radio" aria-checked={item.current_status===status} accessibilityState={{checked:item.current_status===status}} style={[ui.outlineButton,item.current_status===status&&{borderColor:"#34BB84",backgroundColor:"#EFFFF8"}]}><Text style={{fontSize:12, lineHeight: 18}}>{labels[status]}</Text></Pressable>)}</View>
<TextInput multiline value={memo??item.memo??""} onChangeText={setMemo} placeholder="접수 번호, 결과 등 나만의 메모" maxLength={2000} style={{padding:14,minHeight:100,borderRadius:12,backgroundColor:"#F7F9FA",fontFamily:"Pretendard",fontSize:13, lineHeight: 20}}/><Pressable disabled={mutation.isPending} onPress={()=>mutation.mutate(item.current_status)} style={[ui.primaryButton,{marginVertical:12}]}><Text style={ui.primaryButtonText}>메모 저장</Text></Pressable>{mutation.isError&&<Text style={{color:"#F75B2B"}}>저장하지 못했어요. 다시 시도해 주세요.</Text>}
<View style={{marginTop:16,gap:12}}><Text style={ui.cardTitle}>변경 이력</Text>{item.history.map((h,i)=><View key={i}><Text style={{fontSize:13, lineHeight: 20}}>{labels[h.state]}</Text><Text style={ui.muted}>{new Date(h.changed_at).toLocaleString("ko-KR")}{h.memo?` · ${h.memo}`:""}</Text></View>)}</View></>:<EmptyState loading={query.isLoading} title={query.isError?"신청 이력을 불러오지 못했어요":"보관된 신청 이력이 없어요"} action={query.isError?"다시 시도":undefined} onAction={()=>void query.refetch()}/>}
</ScrollView></View>}
