import DefaultTheme from "vitepress/theme";
import "./custom.css";

import HomeHero from "./components/HomeHero.vue";
import HomeFeatures from "./components/HomeFeatures.vue";
import HomeArchitecture from "./components/HomeArchitecture.vue";
import HomeQuickStart from "./components/HomeQuickStart.vue";
import HomeCTA from "./components/HomeCTA.vue";

export default {
  extends: DefaultTheme,
  enhanceApp({ app }: { app: import("vue").App }) {
    app.component("HomeHero", HomeHero);
    app.component("HomeFeatures", HomeFeatures);
    app.component("HomeArchitecture", HomeArchitecture);
    app.component("HomeQuickStart", HomeQuickStart);
    app.component("HomeCTA", HomeCTA);
  },
};
