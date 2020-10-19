//! Does all initialization oriented things

use imxrt_ral as ral;
use super::CANFD;
use super::config;
use super::can_error::CANFDError;

impl CANFD {
    pub fn init_clocks(&mut self, ccm: &ral::ccm::Instance) {
        // Init clock source to 24MHz for now
        ral::modify_reg!(ral::ccm, ccm, CSCMR2, CAN_CLK_SEL: 0b01, CAN_CLK_PODF: 0b00);

        // Due to a hardware bug, the LPUART clock must be on for CanFD to work
        ral::modify_reg!(ral::ccm, ccm, CCGR0, CG6: 0b11);
    
        // Enable clocks
        ral::modify_reg!(ral::ccm, ccm, CCGR7, CG4: 0b11, CG3: 0b11);
    }

    pub fn init_pins(&mut self, iomuxc: &ral::iomuxc::Instance) {
        // Set transfer pin
        ral::modify_reg!(ral::iomuxc, iomuxc, SW_MUX_CTL_PAD_GPIO_EMC_36, SION: 0b1, MUX_MODE: 0b1001); 
        ral::modify_reg!(ral::iomuxc, iomuxc, SW_PAD_CTL_PAD_GPIO_EMC_36, |_| 0x10B0);

        // Set receive pin
        ral::modify_reg!(ral::iomuxc, iomuxc, CANFD_IPP_IND_CANRX_SELECT_INPUT, DAISY: 0b00);
        ral::modify_reg!(ral::iomuxc, iomuxc, SW_MUX_CTL_PAD_GPIO_EMC_37, SION: 0b1, MUX_MODE: 0b1001); 
        ral::modify_reg!(ral::iomuxc, iomuxc, SW_PAD_CTL_PAD_GPIO_EMC_37, |_| 0x10B0);
    }

    pub fn init(&mut self) -> Result<(), CANFDError> {
        if let Err(err) = self.init_classical() {
            return Err(err);
        }

        if let Err(err) = self.init_fd() {
            return Err(err);
        }

        return Ok(());
    }

    fn init_classical(&mut self) -> Result<(), CANFDError> {
        self.enable(true);
        self.reset();

        // Disable loop back (LPB) & listen only (LOM) & timer sync (TSYN)
        ral::modify_reg!(ral::can3, self.instance, CTRL1, LPB: 0b0, LOM: 0b0, TSYN: 0b0);

        // Set:         Maximum # of message buffers (from region sizes)
        // Disable:     Self wakeup (SLFWAK)
        // Disable:     Wake up source (WAKSRC), not used because SLFWAK is disabled
        // Enable:      Individual RX masking & queues (IRMQ), basically global vs local rx masking
        // Disable:     Self-reception (SRXDIS)
        // Disable:     Doze mode (DOZE)
        // Enable:      Transmission abort (AEN)     
        ral::modify_reg!(ral::can3, self.instance, MCR, 
            MAXMB: (self.get_max_message_buffers() - 1) & 0x7F, SLFWAK: 0b0, WAKSRC: 0b0,
            IRMQ: 0b1, SRXDIS: 0b0, DOZE: 0b0, AEN: 0b1);

        // --- Set timing config for classical CAN --- //

        let timing = &self.config.timing_classical;

        if timing.baudrate > config::MAX_BAUDRATE_CLASSICAL {
            return Err(CANFDError::BaudrateTooHigh);
        }

        let quantum = 4 + timing.phase_seg_1 + timing.phase_seg_2 + timing.prop_seg;

        let mut pre_div = timing.baudrate * (quantum as u32);

        if pre_div > self.config.clock_speed.to_hz() {
            return Err(CANFDError::PrescalarTooHigh);
        }
    
        pre_div = ((self.config.clock_speed.to_hz() / pre_div.max(1)) - 1).min(0xFF);

        let rjw = timing.jump_width as u32;
        let seg1 = timing.phase_seg_1 as u32;
        let seg2 = timing.phase_seg_2 as u32;
        let prop_seg = timing.prop_seg as u32;
    
        self.enter_freeze();
    
        // Write timing config to register
        ral::modify_reg!(ral::can3, self.instance, CBT, EPRESDIV: pre_div, ERJW: rjw, EPSEG1: seg1, 
            EPSEG2: seg2, EPROPSEG:prop_seg);
        
        self.exit_freeze();

        return Ok(());
    }

    fn init_fd(&mut self) -> Result<(), CANFDError> {
        // --- Set timing config for CAN FD--- //

        let timing = &self.config.timing_fd;

        if timing.baudrate > config::MAX_BAUDRATE_FD {
            return Err(CANFDError::BaudrateTooHigh);
        }

        let quantum = 4 + timing.phase_seg_1 + timing.phase_seg_2 + timing.prop_seg;

        let mut fpre_div = timing.baudrate * (quantum as u32);

        if fpre_div > self.config.clock_speed.to_hz() {
            return Err(CANFDError::PrescalarTooHigh);
        }
    
        fpre_div = ((self.config.clock_speed.to_hz() / fpre_div.max(1)) - 1).min(0xFF);
    
        let frjw = timing.jump_width as u32;
        let fseg1 = timing.phase_seg_1 as u32;
        let fseg2 = timing.phase_seg_2 as u32;
        let fprop_seg = timing.prop_seg as u32;
        let tdcen = if self.config.transceiver_compensation { 0b1 } else { 0b0 };
        let tdcoff = (self.config.clock_speed.to_hz() / (2 * timing.baudrate / 1000)).max(1);
    
        if self.config.transceiver_compensation && tdcoff > 0b1111 {
            return Err(CANFDError::TransceiverDelayCompensationTooHigh);
        }

        self.enter_freeze();
    
        // Write timing config to register
        ral::modify_reg!(ral::can3, self.instance, FDCBT, FPRESDIV: fpre_div, FRJW: frjw, 
            FPSEG1: fseg1, FPSEG2: fseg2, FPROPSEG:fprop_seg);

        // Enable: CAN FD
        ral::modify_reg!(ral::can3, self.instance, MCR, FDEN: 0b1);

        // Enable:      Bit rate switch enable (FDRATE), enables faster bitrates in FD
        // Set:         Transceiver delay compensation (TDCOFF), shouldn't matter if disabled
        // Set:         Transceiver delay compensation enable (TDCEN)
        // Set:         Message buffer data size region 1 (MBDSR0), size of MBs in RAM region 1
        // Set:         Message buffer data size region 2 (MBDSR1), size of MBs in RAM region 2
        ral::modify_reg!(ral::can3, self.instance, FDCTRL, FDRATE: 0b1, TDCOFF: tdcoff, TDCEN: tdcen, 
            MBDSR0: self.config.region_1_mb_size.to_mbdsr_n(), 
            MBDSR1: self.config.region_2_mb_size.to_mbdsr_n());

        self.exit_freeze();

        // Check to see if we failed TDC
        if ral::read_reg!(ral::can3, self.instance, FDCTRL, TDCFAIL) == 0b1 {
            return Err(CANFDError::TransceiverDelayCompensationFail);
        }

        return Ok(());
    }
}