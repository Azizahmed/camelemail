import "./app.css";
// Phone styles: scoped to html.mobile and the .m-* classes of src/mobile.
import "./mobile/mobile.css";
import { mount } from "svelte";
import App from "./components/App.svelte";

const app = mount(App, { target: document.getElementById("app")! });

export default app;
