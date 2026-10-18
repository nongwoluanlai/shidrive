import { mount } from "svelte";
import "./web.css";
import WebApp from "./WebApp.svelte";

const target = document.getElementById("sd-web")!;
export default mount(WebApp, { target });
