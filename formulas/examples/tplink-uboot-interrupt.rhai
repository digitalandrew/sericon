// TP-Link MT7628: arm this interactive formula, then power-cycle the router.
// Only tpl bursts and line cleanup; no relay control or flash operations.
mark();
emit(#{stage: "armed", message: "Waiting for a fresh U-Boot banner; power-cycle the router"});
let boot = expect_text("U-Boot ", 60000);
if !boot.matched { throw "No fresh U-Boot banner within 60 seconds"; }

let prompt_pattern = "MT7628 #[ ]*";
// Each 768-byte burst spans about 67 ms at the tested 115200 baud.
let burst = "";
for chunk in 0..256 { burst += "tpl"; }
for attempt in 1..101 {
    send(burst);
    let reply = expect(prompt_pattern + "|Starting kernel|Booting image", 50);
    if reply.matched {
        if reply.text.contains("Starting kernel") || reply.text.contains("Booting image") {
            throw "Router continued booting; interrupt window was missed";
        }
        // Allow the bounded burst to finish, then clear any echoed tail.
        sleep_ms(1000);
        mark();
        send_bytes(hex_decode("15 0d"));
        let clean_prompt = expect(prompt_pattern, 2000);
        if !clean_prompt.matched { throw "Prompt did not return after line cleanup"; }
        release();
        emit(#{stage: "interrupted", attempts: attempt, prompt: utf8(reply.match_bytes), cursor: clean_prompt.cursor});
        return;
    }
}
throw "No U-Boot prompt after 100 tpl bursts; inspect UART and recover input ownership";
