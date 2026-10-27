// 桌宠窗口（pet）：透明置顶小窗，显示皮肤人物与对话气泡；逻辑在主窗口（lib/assistant/pet.svelte.ts）
import { mount } from "svelte";
import "../skin-vars.css";
import "../themes.css";
import "./pet.css";
import Pet from "./Pet.svelte";

export default mount(Pet, { target: document.getElementById("pet")! });
