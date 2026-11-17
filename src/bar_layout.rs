//! 常驻组件按实际占位排布，并在开关变化时连续补位。
use crate::anim::{smoothstep, Spring};

pub(crate) const CLOCK: usize = 0;
pub(crate) const BATTERY: usize = 1;
pub(crate) const QUOTA: usize = 2;
/// 可独立排列的天气徽标。
pub(crate) const WEATHER: usize = 5;
pub(crate) fn quota_slot(position:usize)->usize {
    if position<3 {QUOTA+position} else {WIDTHS.len()+position-3}
}
pub(crate) const MUSIC: usize = 6;
pub(crate) const FOCUS: usize = 7;
pub(crate) const BELL: usize = 8;
pub(crate) const PET: usize = 9;
/// 天气徽标的基础槽宽；温度文字更长时由 `widen` 补足。
pub(crate) const WEATHER_WIDTH: f32 = 38.0;
/// 小工具常驻指示（Claude 等待、会议、专注、睿频）：最多两个小图标，作为一组排布。
/// 放在固定槽位里，超过 3 个的额度圆环仍从 WIDTHS.len() 之后追加，互不占用。
pub(crate) const TOOLS: usize = 10;
pub(crate) const TOOL_SLOTS: usize = 2;
const TOOL_W: f32 = 16.0;
pub(crate) const TOOL_STEP: f32 = 23.0;
const WIDTHS: [f32; 12] = [44.0,20.0,27.0,27.0,27.0,WEATHER_WIDTH,14.0,crate::focus_bar::WIDTH,20.0,32.0,TOOL_W,TOOL_W];

pub(crate) const DEFAULT_ORDER: [usize;8]=[BATTERY,PET,CLOCK,WEATHER,QUOTA,MUSIC,FOCUS,TOOLS];
pub(crate) fn valid_order(order:&[usize])->bool {
    order.len()==DEFAULT_ORDER.len() && DEFAULT_ORDER.iter().all(|slot|order.iter().filter(|value|*value==slot).count()==1)
}
/// 旧版本保存的排序缺哪个组件就补哪个：天气插在时钟后面（上游规则），小工具接在最后。
/// 覆盖上游旧版（6 项）、上游新版（7 项含天气）和本分支旧版（7 项含小工具）。
pub(crate) fn upgrade_order(mut order:Vec<usize>)->Vec<usize> {
    if !order.contains(&WEATHER) {
        if let Some(clock)=order.iter().position(|slot|*slot==CLOCK){order.insert(clock+1,WEATHER);}
    }
    if !order.contains(&TOOLS) {order.push(TOOLS);}
    order
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

#[derive(Clone, Debug)]
pub(crate) struct Layout {
    pub width: f32,
    pub x: Vec<Option<f32>>,
    pub widths:Vec<f32>,
    quotas:usize,
}

impl Layout {
    /// `tools` 是要常驻的小工具个数；放不下时（总宽会超过展开面板）整组让位。
    #[allow(clippy::too_many_arguments)]
    pub fn new(battery:bool,quotas:usize,music:bool,focus:bool,bell:bool,pet:bool,weather:bool,tools:usize)->Self {Self::build(battery,quotas,music,focus,bell,pet,weather,tools,crate::app::PW)}
    /// 设置页的排序预览：所有组件都要能拖，不受展开面板宽度限制（预览会整体缩放）。
    pub fn preview(battery:bool,quotas:usize,music:bool,focus:bool,pet:bool,weather:bool,tools:usize)->Self {Self::build(battery,quotas,music,focus,false,pet,weather,tools,f32::INFINITY)}
    #[allow(clippy::too_many_arguments)]
    fn build(battery:bool,quotas:usize,music:bool,focus:bool,bell:bool,pet:bool,weather:bool,tools:usize,limit:f32)->Self {
        fn place(cursor:&mut f32,width:f32)->f32 {let x=*cursor+width/2.0;*cursor+=width+12.0;x}
        let mut widths=WIDTHS.to_vec(); widths.resize(WIDTHS.len()+quotas.saturating_sub(3),27.0);
        let mut x=vec![None;widths.len()];let mut cursor=16.0;
        let count=quotas;
        let order=if battery {[BATTERY,CLOCK,QUOTA]}else{[QUOTA,CLOCK,BATTERY]};
        for slot in order {
            match slot {
                BATTERY if battery=>x[slot]=Some(place(&mut cursor,WIDTHS[slot])),
                CLOCK=>{
                    if pet {x[PET]=Some(place(&mut cursor,WIDTHS[PET]));}
                    x[slot]=Some(place(&mut cursor,WIDTHS[slot]));
                    if weather {x[WEATHER]=Some(place(&mut cursor,WIDTHS[WEATHER]));}
                },
                QUOTA if count>0=>{
                    let width=count as f32*27.0+(count-1) as f32*7.0;
                    let left=place(&mut cursor,width)-width/2.0;
                    for i in 0..count {x[quota_slot(i)]=Some(left+13.5+i as f32*34.0);}
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
        Self {width:width.max(96.0),x,widths,quotas}
    }
    pub fn quota_slots(&self)->impl Iterator<Item=usize> { (0..self.quotas).map(quota_slot) }
    /// 把某个槽加宽 extra：其余槽对称让位并记入总宽。点数文字、时段时钟、天气温度共用。
    pub fn widen(&mut self,slot:usize,extra:f32){
        if extra<=0.0 {return;}
        let Some(center)=self.x.get(slot).copied().flatten() else {return;};
        for (i,x) in self.x.iter_mut().enumerate(){if i!=slot{if let Some(x)=x{*x+=if *x<center{-extra/2.0}else{extra/2.0};}}}
        self.widths[slot]+=extra;self.width+=extra;
    }
    pub fn balance_tail(&mut self,position:usize,extra:f32){self.widen(quota_slot(position),extra);}
    pub fn widen_clock(&mut self, extra:f32) {self.widen(CLOCK,extra);}
    pub fn hide_clock(&mut self) {
        let Some(center)=self.x[CLOCK].take() else{return;};
        let removed=self.widths[CLOCK]+12.0;
        self.width=(self.width-removed).max(96.0);
        for x in self.x.iter_mut().flatten() {*x+=if *x<center {removed/2.0}else{-removed/2.0};}
    }
    pub fn reorder(&mut self,order:&[usize]) {
        if !valid_order(order){return;}
        let original=self.x.clone();let mut cursor=-self.width/2.0+16.0;
        for slot in order.iter().copied().chain(std::iter::once(BELL)) {
            let slots:Vec<usize>=match slot {QUOTA=>self.quota_slots().collect(),TOOLS=>(TOOLS..TOOLS+TOOL_SLOTS).collect(),_=>vec![slot]};
            let left=slots.iter().filter_map(|&i|original[i].map(|x|x-self.widths[i]/2.0)).fold(f32::INFINITY,f32::min);
            let right=slots.iter().filter_map(|&i|original[i].map(|x|x+self.widths[i]/2.0)).fold(f32::NEG_INFINITY,f32::max);
            if !left.is_finite(){continue;}
            for i in slots {if let Some(x)=original[i]{self.x[i]=Some(cursor+x-left);}}
            cursor+=right-left+12.0;
        }
    }
    /// 圆环换边经过时钟时，暂时淡出时钟，避免移动中相互遮挡。
    pub fn clearance(&self,slot:usize)->f32 {
        let Some(x)=self.x[slot] else{return 0.0;};
        let distance=self.quota_slots().filter_map(|i|self.x[i].map(|other|(other-x).abs()-(self.widths[i]-27.0)/2.0)).fold(f32::INFINITY,f32::min);
        let quota=if slot==CLOCK {smoothstep(34.0,44.0,distance)}else{smoothstep(22.0,34.0,distance)};
        quota*self.pet_clearance(slot)
    }
    pub fn pet_clearance(&self,slot:usize)->f32 {
        match (self.x[slot],self.x[PET]) {
            (Some(x),Some(pet))=>smoothstep(self.widths[slot]/2.0+14.0,self.widths[slot]/2.0+24.0,(x-pet).abs()),
            _=>1.0,
        }
    }
}

pub(crate) struct Motion {
    pub target: Layout,
    positions: Vec<Spring>,
    initialized: bool,
}
impl Default for Motion {
    fn default()->Self {Self {target:Layout::new(false,0,false,false,false,false,false,0),positions:vec![Spring::new(0.0,360.0,0.76);WIDTHS.len()],initialized:false}}
}
impl Motion {
    pub fn step(&mut self,target:Layout,dt:f32,animate:bool) {
        self.positions.resize(target.x.len(),Spring::new(0.0,360.0,0.76));
        for (i,x) in target.x.iter().enumerate() {
            if let Some(x)=x {
                let spring=&mut self.positions[i];spring.target=*x;
                if !self.initialized || self.target.x.get(i).copied().flatten().is_none() || !animate {spring.snap(*x);}else{spring.step(dt);}
            }
        }
        self.target=target;self.initialized=true;
    }
    pub fn current(&self)->Layout {
        let mut layout=self.target.clone();
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
    fn hidden_clock_closes_its_gap_in_custom_orders(){
        for reverse in [false,true] {for count in [0,5] {
            let mut order=DEFAULT_ORDER.to_vec();if reverse {order.reverse();}
            let mut layout=Layout::new(true,count,true,true,false,true,true,2);
            layout.reorder(&order);layout.widen_clock(64.0);
            let width=layout.width;let removed=layout.widths[CLOCK]+12.0;
            layout.hide_clock();assert!(layout.x[CLOCK].is_none());
            assert_eq!(layout.width,(width-removed).max(96.0));
            let mut boxes:Vec<_>=layout.x.iter().enumerate().filter_map(|(i,x)|x.map(|x|(x-layout.widths[i]/2.0,x+layout.widths[i]/2.0))).collect();
            boxes.sort_by(|a,b|a.0.total_cmp(&b.0));
            assert!(boxes.windows(2).all(|p|p[1].0-p[0].1>=6.9),"关闭时间后相邻组件不应重叠");
            let after=layout.x.clone();layout.hide_clock();assert_eq!(layout.x,after);
        }}
        let mut empty=Layout::new(false,0,false,false,false,false,false,0);
        empty.hide_clock();assert_eq!(empty.width,96.0);assert!(empty.x.iter().all(Option::is_none));
    }
    #[test]
    fn weather_can_be_reordered_independently_of_the_clock(){
        let order=[WEATHER,FOCUS,MUSIC,QUOTA,CLOCK,PET,BATTERY,TOOLS];
        assert!(valid_order(&order),"天气应有独立的排列位置");
        for quotas in 0..=5 {for enabled in [false,true] {
            let mut layout=Layout::new(true,quotas,true,true,false,true,enabled,2);
            layout.reorder(&order);layout.widen(WEATHER,18.0);layout.widen_clock(40.0);
            let positions:Vec<_>=order.iter().filter_map(|slot|layout.x[*slot]).collect();
            assert!(positions.windows(2).all(|pair|pair[0]<pair[1]));
            let boxes:Vec<_>=order.iter().flat_map(|&slot|if slot==QUOTA{layout.quota_slots().collect()}else{vec![slot]}).filter_map(|slot|layout.x[slot].map(|x|(x-layout.widths[slot]/2.0,x+layout.widths[slot]/2.0))).collect();
            assert!(boxes.windows(2).all(|pair|pair[1].0-pair[0].1>=6.9),"排序和加宽后组件不能重叠");
        }}
    }
    #[test] fn deepseek_balance_follows_more_and_never_overlaps(){
        for order in [DEFAULT_ORDER,[FOCUS,MUSIC,QUOTA,CLOCK,WEATHER,PET,BATTERY,TOOLS]] {for quotas in 1..=6 {for battery in [false,true]{
            let mut layout=Layout::new(battery,quotas,true,true,false,true,false,2);layout.reorder(&order);layout.balance_tail(quotas-1,76.0);layout.widen_clock(40.0);
            let mut boxes:Vec<_>=layout.x.iter().enumerate().filter_map(|(i,x)|x.map(|x|{let width=layout.widths[i];(x-width/2.0,x+width/2.0)})).collect();boxes.sort_by(|a,b|a.0.total_cmp(&b.0));
            assert!(boxes.windows(2).all(|p|p[1].0-p[0].1>=6.9));assert!(boxes.last().unwrap().1<=layout.width/2.0-15.9);
            assert!(layout.width<crate::app::WIN_W);
        }}}
    }
    #[test]
    fn clock_12h_keeps_components_separate_in_custom_orders(){
        for order in [DEFAULT_ORDER,[FOCUS,MUSIC,QUOTA,CLOCK,WEATHER,PET,BATTERY,TOOLS]] {
            for quotas in 0..=5 {
                let mut layout=Layout::new(true,quotas,true,true,false,true,false,2);
                layout.reorder(&order);layout.widen_clock(40.0);
                let mut boxes:Vec<_>=layout.x.iter().enumerate().filter_map(|(i,x)|x.map(|x|{
                    let width=layout.widths[i];
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
        assert!(!valid_order(&[0,0,2,6,7,9]));
        assert!(!valid_order(&[0,1,2]));
        let order=[FOCUS,MUSIC,QUOTA,CLOCK,WEATHER,PET,BATTERY,TOOLS];
        for battery in [false,true]{for count in 0..=5 {
            let mut layout=Layout::new(battery,count,true,true,false,true,false,2);
            let width=layout.width;layout.reorder(&order);
            assert_eq!(layout.width,width);
            let positions:Vec<_>=order.iter().filter_map(|i|layout.x[*i]).collect();
            assert!(positions.windows(2).all(|p|p[0]<p[1]));
            if count>1 {assert_eq!(layout.x[QUOTA+1].unwrap()-layout.x[QUOTA].unwrap(),34.0);}
            let mut boxes:Vec<_>=layout.x.iter().enumerate().filter_map(|(i,x)|x.map(|x|(x-layout.widths[i]/2.0,x+layout.widths[i]/2.0))).collect();
            boxes.sort_by(|a,b|a.0.total_cmp(&b.0));
            assert!(boxes.windows(2).all(|p|p[1].0-p[0].1>=6.9));
        }}
    }
    #[test]
    fn every_component_combination_fits_without_overlaps(){
        for quotas in 0..=8 {for battery in [false,true] {for music in [false,true] {for focus in [false,true] {for bell in [false,true] {for pet in [false,true] {for weather in [false,true] {for tools in 0..=3 {
            let layout=Layout::new(battery,quotas,music,focus,bell,pet,weather,tools);
            let mut boxes:Vec<_>=layout.x.iter().enumerate().filter_map(|(i,x)|x.map(|x|(x-layout.widths[i]/2.0,x+layout.widths[i]/2.0))).collect();
            boxes.sort_by(|a,b|a.0.total_cmp(&b.0));
            assert_eq!(layout.quota_slots().count(),quotas);
            assert!(boxes.first().unwrap().0>=-layout.width/2.0+15.9);
            assert!(boxes.last().unwrap().1<=layout.width/2.0-15.9);
            assert!(boxes.windows(2).all(|pair|pair[1].0-pair[0].1>=6.9),"组件不能挤在一起：{layout:?}");
            assert_eq!(layout.clearance(CLOCK),1.0);
            if tools>0 && quotas<=2 && !weather {assert!(layout.x[TOOLS].is_some(),"空间够时小工具应常驻：{layout:?}");}
            if layout.x[TOOLS].is_some() {assert!(layout.width<=crate::app::PW,"小工具不能把收起的岛撑得比展开面板还宽");}
        }}}}}}}}
    }
    #[test]
    fn preview_always_keeps_tools_draggable(){
        for quotas in 0..=8 {
            let layout=Layout::preview(true,quotas.max(1),true,true,true,true,TOOL_SLOTS);
            assert!(layout.x[TOOLS].is_some()&&layout.x[TOOLS+1].is_some());
        }
    }
    #[test]
    fn extra_quota_rings_never_reuse_tool_slots(){
        let layout=Layout::new(true,6,false,false,false,false,true,2);
        assert!(layout.quota_slots().all(|slot|!(TOOLS..TOOLS+TOOL_SLOTS).contains(&slot)&&slot!=WEATHER));
    }
    #[test]
    fn saved_orders_from_every_version_upgrade_to_the_full_list(){
        // 上游旧版（6 项）、上游新版（7 项含天气）、本分支旧版（7 项含小工具）
        assert_eq!(upgrade_order(vec![BATTERY,PET,CLOCK,QUOTA,MUSIC,FOCUS]),vec![BATTERY,PET,CLOCK,WEATHER,QUOTA,MUSIC,FOCUS,TOOLS]);
        assert_eq!(upgrade_order(vec![FOCUS,CLOCK,WEATHER,PET,BATTERY,QUOTA,MUSIC]),vec![FOCUS,CLOCK,WEATHER,PET,BATTERY,QUOTA,MUSIC,TOOLS]);
        assert_eq!(upgrade_order(vec![TOOLS,FOCUS,MUSIC,QUOTA,CLOCK,PET,BATTERY]),vec![TOOLS,FOCUS,MUSIC,QUOTA,CLOCK,WEATHER,PET,BATTERY]);
        for order in [vec![BATTERY,PET,CLOCK,QUOTA,MUSIC,FOCUS],vec![TOOLS,FOCUS,MUSIC,QUOTA,CLOCK,PET,BATTERY],DEFAULT_ORDER.to_vec()] {assert!(valid_order(&upgrade_order(order)));}
        assert_eq!(upgrade_order(DEFAULT_ORDER.to_vec()),DEFAULT_ORDER.to_vec(),"完整的排序不改动");
    }
    #[test]
    fn weather_defaults_after_the_clock_and_preserves_custom_spacing(){
        for order in [DEFAULT_ORDER,[FOCUS,MUSIC,QUOTA,CLOCK,WEATHER,PET,BATTERY,TOOLS],[QUOTA,MUSIC,CLOCK,WEATHER,PET,FOCUS,BATTERY,TOOLS]] {
            for quotas in 0..=3 {for battery in [false,true] {
                let mut layout=Layout::new(battery,quotas,true,true,false,true,true,2);
                layout.reorder(&order);
                let (clock,weather)=match (layout.x[CLOCK],layout.x[WEATHER]) {(Some(c),Some(w))=>(c,w),_=>panic!("时钟与天气都应就位：{layout:?}")};
                assert!(weather>clock+22.0+6.9,"天气应紧跟时钟右侧：时钟 {clock}，天气 {weather}");
                // 相邻排列的时钟与天气保持标准组件间距。
                assert!((weather-clock-(38.0+44.0)/2.0-12.0).abs()<0.01);
                let mut boxes:Vec<_>=layout.x.iter().enumerate().filter_map(|(i,x)|x.map(|x|(x-layout.widths[i]/2.0,x+layout.widths[i]/2.0))).collect();
                boxes.sort_by(|a,b|a.0.total_cmp(&b.0));
                assert!(boxes.windows(2).all(|p|p[1].0-p[0].1>=6.9));
                assert!(boxes.last().unwrap().1<=layout.width/2.0-15.9);
                assert!(layout.clearance(WEATHER)>0.99,"天气与额度环不应对撞：{layout:?}");
            }}
        }
    }
    #[test]
    fn weather_chip_widens_symmetrically_without_overlaps(){
        let mut layout=Layout::new(true,3,true,true,false,true,true,2);
        let before=layout.x.clone();
        layout.widen(WEATHER,18.0);
        assert!((layout.widths[WEATHER]-56.0).abs()<0.01);
        for (slot,x) in layout.x.iter().enumerate() {
            match (slot,x) {
                (crate::bar_layout::WEATHER,Some(_))=>{},
                (_,Some(x))=>assert!((*x-before[slot].unwrap()).abs()>8.0,"其余槽位应被推开：{slot}"),
                (_,None)=>{},
            }
        }
        let mut boxes:Vec<_>=layout.x.iter().enumerate().filter_map(|(i,x)|x.map(|x|(x-layout.widths[i]/2.0,x+layout.widths[i]/2.0))).collect();
        boxes.sort_by(|a,b|a.0.total_cmp(&b.0));
        assert!(boxes.windows(2).all(|p|p[1].0-p[0].1>=6.9));
        // 未就位的槽位调用 widen 应是空操作。
        let mut empty=Layout::new(true,0,true,true,false,true,false,0);
        let width=empty.width;empty.widen(WEATHER,20.0);
        assert_eq!(empty.width,width);
    }
    #[test]
    fn replacing_battery_moves_continuously_and_supports_an_instant_state(){
        let before=Layout::new(true,1,true,false,false,true,false,2);let after=Layout::new(false,1,true,false,false,true,false,2);
        let mut motion=Motion::default();motion.step(before.clone(),0.0,true);
        motion.step(after.clone(),1.0/60.0,true);let mid=motion.current().x[QUOTA].unwrap();
        assert!(mid<before.x[QUOTA].unwrap()&&mid>after.x[QUOTA].unwrap());
        for _ in 0..120 {motion.step(after.clone(),1.0/60.0,true);}
        assert!((motion.current().x[QUOTA].unwrap()-after.x[QUOTA].unwrap()).abs()<0.01);
        motion.step(before.clone(),1.0/60.0,false);assert_eq!(motion.current().x,before.x);
    }
}
