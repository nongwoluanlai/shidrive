import { mount } from "svelte";
import "./app.css";
import "./skin-vars.css";
import "./themes.css";
import App from "./App.svelte";

const target = document.getElementById("app")!;
export default mount(App, { target });
