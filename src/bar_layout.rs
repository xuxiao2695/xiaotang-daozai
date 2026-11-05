//! 常驻组件按实际占位排布，并在开关变化时连续补位。
use crate::anim::{smoothstep, Spring};

pub(crate) const CLOCK: usize = 0;
pub(crate) const BATTERY: usize = 1;
pub(crate) const QUOTA: usize = 2;
pub(crate) const MORE: usize = 5;
pub(crate) const MUSIC: usize = 6;
pub(crate) const FOCUS: usize = 7;
pub(crate) const BELL: usize = 8;
pub(crate) const PET: usize = 9;
/// 小工具常驻指示（Claude 等待、会议、专注、睿频）：最多两个小图标，作为一组排布。
pub(crate) const TOOLS: usize = 10;
pub(crate) const TOOL_SLOTS: usize = 2;
const SLOTS: usize = 12;
const TOOL_W: f32 = 16.0;
pub(crate) const TOOL_STEP: f32 = 23.0;
const WIDTHS: [f32; SLOTS] = [44.0,20.0,27.0,27.0,27.0,22.0,14.0,crate::focus_bar::WIDTH,20.0,32.0,TOOL_W,TOOL_W];

pub(crate) const DEFAULT_ORDER: [usize;7]=[BATTERY,PET,CLOCK,QUOTA,MUSIC,FOCUS,TOOLS];
pub(crate) fn valid_order(order:&[usize])->bool {
    order.len()==DEFAULT_ORDER.len() && DEFAULT_ORDER.iter().all(|slot|order.iter().filter(|value|*value==slot).count()==1)
}
/// 旧版本保存的 6 个组件顺序：保留原排列，把小工具接在最后。
pub(crate) fn upgrade_order(mut order:Vec<usize>)->Vec<usize> {
    if order.len()==DEFAULT_ORDER.len()-1 && !order.contains(&TOOLS) {order.push(TOOLS);}
    order
}
fn group(slot:usize)->std::ops::RangeInclusive<usize> {
    match slot {QUOTA=>QUOTA..=MORE,TOOLS=>TOOLS..=TOOLS+TOOL_SLOTS-1,_=>slot..=slot}
}

pub(crate) fn animations_enabled()->bool {
    #[cfg(windows)] {
        static ENABLED:std::sync::OnceLock<bool>=std::sync::OnceLock::new();
        *ENABLED.get_or_init(||{
            use windows::Win32::{Foundation::BOOL,UI::WindowsAndMessaging::*};
            let mut enabled=BOOL(1);
            unsafe {let _=SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION,0,Some((&mut enabled as *mut BOOL).cast()),SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));}
            enabled.as_bool()
        })
    }
    #[cfg(not(windows))] {true}
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    pub width: f32,
    pub x: [Option<f32>; SLOTS],
}

impl Layout {
    /// `tools` 是要常驻的小工具个数；放不下时（总宽会超过展开面板）整组让位。
    pub fn new(battery:bool,quotas:usize,music:bool,focus:bool,bell:bool,pet:bool,tools:usize)->Self {Self::build(battery,quotas,music,focus,bell,pet,tools,crate::app::PW)}
    /// 设置页的排序预览：所有组件都要能拖，不受展开面板宽度限制（预览会整体缩放）。
    pub fn preview(battery:bool,quotas:usize,music:bool,focus:bool,pet:bool,tools:usize)->Self {Self::build(battery,quotas,music,focus,false,pet,tools,f32::INFINITY)}
    fn build(battery:bool,quotas:usize,music:bool,focus:bool,bell:bool,pet:bool,tools:usize,limit:f32)->Self {
        fn place(cursor:&mut f32,width:f32)->f32 {let x=*cursor+width/2.0;*cursor+=width+12.0;x}
        let mut x=[None;SLOTS];let mut cursor=16.0;
        let count=quotas.min(3);
        let order=if battery {[BATTERY,CLOCK,QUOTA]}else{[QUOTA,CLOCK,BATTERY]};
        for slot in order {
            match slot {
                BATTERY if battery=>x[slot]=Some(place(&mut cursor,WIDTHS[slot])),
                CLOCK=>{
                    if pet {x[PET]=Some(place(&mut cursor,WIDTHS[PET]));}
                    x[slot]=Some(place(&mut cursor,WIDTHS[slot]));
                },
                QUOTA if count>0=>{
                    let width=count as f32*27.0+(count-1) as f32*7.0+if quotas>3{29.0}else{0.0};
                    let left=place(&mut cursor,width)-width/2.0;
                    for i in 0..count {x[QUOTA+i]=Some(left+13.5+i as f32*34.0);}
                    if quotas>3 {x[MORE]=Some(left+count as f32*34.0+11.0);}
                },
                _=>{},
            }
        }
        for (slot,on) in [(MUSIC,music&&!bell),(FOCUS,focus)] {
            if on {x[slot]=Some(place(&mut cursor,WIDTHS[slot]));}
        }
        let tools=tools.min(TOOL_SLOTS);
        if tools>0 {
            let width=TOOL_W+(tools-1) as f32*TOOL_STEP;
            let bell_width=if bell {WIDTHS[BELL]+12.0}else{0.0};
            if cursor+width+12.0+bell_width-12.0+16.0<=limit {
                let left=place(&mut cursor,width)-width/2.0;
                for i in 0..tools {x[TOOLS+i]=Some(left+TOOL_W/2.0+i as f32*TOOL_STEP);}
            }
        }
        if bell {x[BELL]=Some(place(&mut cursor,WIDTHS[BELL]));}
        let width=cursor-12.0+16.0;
        for value in x.iter_mut().flatten() {*value-=width/2.0;}
        Self {width:width.max(96.0),x}
    }
    pub fn widen_clock(&mut self, extra:f32) {
        let clock=self.x[CLOCK].unwrap();
        for (slot,x) in self.x.iter_mut().enumerate() {
            if slot!=CLOCK { if let Some(x)=x { *x+=if *x<clock {-extra/2.0}else{extra/2.0}; } }
        }
        self.width+=extra;
    }
    pub fn reorder(&mut self,order:&[usize]) {
        if !valid_order(order){return;}
        let original=self.x;let mut cursor=-self.width/2.0+16.0;
        for slot in order.iter().copied().chain(std::iter::once(BELL)) {
            let slots:Vec<usize>=group(slot).collect();
            let left=slots.iter().filter_map(|&i|original[i].map(|x|x-WIDTHS[i]/2.0)).fold(f32::INFINITY,f32::min);
            let right=slots.iter().filter_map(|&i|original[i].map(|x|x+WIDTHS[i]/2.0)).fold(f32::NEG_INFINITY,f32::max);
            if !left.is_finite(){continue;}
            for i in slots {if let Some(x)=original[i]{self.x[i]=Some(cursor+x-left);}}
            cursor+=right-left+12.0;
        }
    }
    /// 圆环换边经过时钟时，暂时淡出时钟，避免移动中相互遮挡。
    pub fn clearance(&self,slot:usize)->f32 {
        let Some(x)=self.x[slot] else{return 0.0;};
        let distance=self.x[QUOTA..=MORE].iter().flatten().map(|other|(other-x).abs()).fold(f32::INFINITY,f32::min);
        let quota=if slot==CLOCK {smoothstep(34.0,44.0,distance)}else{smoothstep(22.0,34.0,distance)};
        quota*self.pet_clearance(slot)
    }
    pub fn pet_clearance(&self,slot:usize)->f32 {
        match (self.x[slot],self.x[PET]) {
            (Some(x),Some(pet))=>smoothstep(WIDTHS[slot]/2.0+14.0,WIDTHS[slot]/2.0+24.0,(x-pet).abs()),
            _=>1.0,
        }
    }
}

pub(crate) struct Motion {
    pub target: Layout,
    positions: [Spring; SLOTS],
    initialized: bool,
}
impl Default for Motion {
    fn default()->Self {Self {target:Layout::new(false,0,false,false,false,false,0),positions:[Spring::new(0.0,360.0,0.76);SLOTS],initialized:false}}
}
impl Motion {
    pub fn step(&mut self,target:Layout,dt:f32,animate:bool) {
        for (i,x) in target.x.iter().enumerate() {
            if let Some(x)=x {
                let spring=&mut self.positions[i];spring.target=*x;
                if !self.initialized || self.target.x[i].is_none() || !animate {spring.snap(*x);}else{spring.step(dt);}
            }
        }
        self.target=target;self.initialized=true;
    }
    pub fn current(&self)->Layout {
        let mut layout=self.target;
        for (i,x) in layout.x.iter_mut().enumerate() {if x.is_some(){*x=Some(self.positions[i].pos);}}
        layout
    }
    pub fn position(&self,slot:usize)->f32 {self.positions[slot].pos}
    pub fn settled(&self)->bool {self.target.x.iter().enumerate().all(|(i,x)|x.is_none()||self.positions[i].settled(100.0))}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clock_12h_keeps_components_separate_in_custom_orders(){
        for order in [DEFAULT_ORDER,[TOOLS,FOCUS,MUSIC,QUOTA,CLOCK,PET,BATTERY]] {
            for quotas in 0..=5 {
                let mut layout=Layout::new(true,quotas,true,true,false,true,2);
                layout.reorder(&order);layout.widen_clock(40.0);
                let mut boxes:Vec<_>=layout.x.iter().enumerate().filter_map(|(i,x)|x.map(|x|{
                    let width=WIDTHS[i]+if i==CLOCK {40.0}else{0.0};
                    (x-width/2.0,x+width/2.0)
                })).collect();
                boxes.sort_by(|a,b|a.0.total_cmp(&b.0));
                assert!(boxes.windows(2).all(|p|p[1].0-p[0].1>=6.9));
                assert!(boxes.first().unwrap().0>=-layout.width/2.0+15.9);
                assert!(boxes.last().unwrap().1<=layout.width/2.0-15.9);
            }
        }
    }
    #[test]
    fn custom_order_preserves_groups_and_hidden_positions(){
        assert!(!valid_order(&[0,0,2,6,7,9,10]));
        assert!(valid_order(&upgrade_order(vec![7,6,2,0,9,1])));
        assert!(!valid_order(&[0,1,2]));
        let order=[FOCUS,TOOLS,MUSIC,QUOTA,CLOCK,PET,BATTERY];
        for battery in [false,true]{for count in 0..=5 {
            let mut layout=Layout::new(battery,count,true,true,false,true,2);
            let width=layout.width;layout.reorder(&order);
            assert_eq!(layout.width,width);
            let positions:Vec<_>=order.iter().filter_map(|i|layout.x[*i]).collect();
            assert!(positions.windows(2).all(|p|p[0]<p[1]));
            if count>1 {assert_eq!(layout.x[QUOTA+1].unwrap()-layout.x[QUOTA].unwrap(),34.0);}
            let mut boxes:Vec<_>=layout.x.iter().enumerate().filter_map(|(i,x)|x.map(|x|(x-WIDTHS[i]/2.0,x+WIDTHS[i]/2.0))).collect();
            boxes.sort_by(|a,b|a.0.total_cmp(&b.0));
            assert!(boxes.windows(2).all(|p|p[1].0-p[0].1>=6.9));
        }}
    }
    #[test]
    fn every_component_combination_fits_without_overlaps(){
        for quotas in 0..=8 {for battery in [false,true] {for music in [false,true] {for focus in [false,true] {for bell in [false,true] {for pet in [false,true] {for tools in 0..=3 {
            let layout=Layout::new(battery,quotas,music,focus,bell,pet,tools);
            let mut boxes:Vec<_>=layout.x.iter().enumerate().filter_map(|(i,x)|x.map(|x|(x-WIDTHS[i]/2.0,x+WIDTHS[i]/2.0))).collect();
            boxes.sort_by(|a,b|a.0.total_cmp(&b.0));
            assert!(layout.width<=crate::app::PW);
            assert!(boxes.first().unwrap().0>=-layout.width/2.0+15.9);
            assert!(boxes.last().unwrap().1<=layout.width/2.0-15.9);
            assert!(boxes.windows(2).all(|pair|pair[1].0-pair[0].1>=6.9),"组件不能挤在一起：{layout:?}");
            assert_eq!(layout.clearance(CLOCK),1.0);
            if tools>0 && quotas<=2 {assert!(layout.x[TOOLS].is_some(),"空间够时小工具应常驻：{layout:?}");}
        }}}}}}}
    }
    #[test]
    fn preview_always_keeps_tools_draggable(){
        for quotas in 0..=8 {
            let layout=Layout::preview(true,quotas.max(1),true,true,true,TOOL_SLOTS);
            assert!(layout.x[TOOLS].is_some()&&layout.x[TOOLS+1].is_some());
            assert_eq!(layout.x[TOOLS+1].unwrap()-layout.x[TOOLS].unwrap(),TOOL_STEP);
        }
    }
    #[test]
    fn replacing_battery_moves_continuously_and_supports_an_instant_state(){
        let before=Layout::new(true,1,true,false,false,true,0);let after=Layout::new(false,1,true,false,false,true,0);
        let mut motion=Motion::default();motion.step(before,0.0,true);
        motion.step(after,1.0/60.0,true);let mid=motion.current().x[QUOTA].unwrap();
        assert!(mid<before.x[QUOTA].unwrap()&&mid>after.x[QUOTA].unwrap());
        for _ in 0..120 {motion.step(after,1.0/60.0,true);}
        assert!((motion.current().x[QUOTA].unwrap()-after.x[QUOTA].unwrap()).abs()<0.01);
        motion.step(before,1.0/60.0,false);assert_eq!(motion.current().x,before.x);
    }
}
