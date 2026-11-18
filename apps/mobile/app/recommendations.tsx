import React, { useEffect, useState } from "react";
import { View } from "react-native";
import { CategoryTabs, ScreenHeader, ui } from "@/components/FigmaUI";
import RecommendationList from "@/components/RecommendationList";
import { useSchoolSelection } from "@/lib/useSchoolSelection";
import { ALL_TAB, CAMPUS_TAB, type CategoryTab } from "@/constants/fields";

/** 추천 혜택 목록 (Figma 34065:5283): back header, tabs, then the recommendation list. */
export default function RecommendationsScreen() {
  const [category, setCategory] = useState<CategoryTab>(ALL_TAB);
  const school = useSchoolSelection();
  const activeCategory = category === CAMPUS_TAB && !school.showCampus ? ALL_TAB : category;
  useEffect(() => { if (!school.showCampus && category === CAMPUS_TAB) setCategory(ALL_TAB); }, [school.showCampus, category]);
  return <View style={ui.screen}>
    <ScreenHeader title="추천 혜택 목록" back tabs />
    <CategoryTabs value={activeCategory} showCampus={school.showCampus} onChange={setCategory} />
    <RecommendationList category={activeCategory} query="" />
  </View>;
}
