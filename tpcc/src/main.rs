use crate::delivery::DeliveryTest;
use crate::load::Load;
use crate::new_ord::NewOrdTest;
use crate::order_stat::OrderStatTest;
use crate::payment::PaymentTest;
use crate::rt_hist::RtHist;
use crate::slev::SlevTest;
use crate::utils::SeqGen;
use clap::Parser;
use kite_sql::db::{DBTransaction, DataBaseBuilder, Statement};
use kite_sql::errors::DatabaseError;
use kite_sql::storage::Storage;
use rand::prelude::ThreadRng;
use rand::Rng;
use std::time::{Duration, Instant};

mod delivery;
mod load;
mod new_ord;
mod order_stat;
mod payment;
mod rt_hist;
mod slev;
mod utils;

pub(crate) const ALLOW_MULTI_WAREHOUSE_TX: bool = true;
pub(crate) const RT_LIMITS: [Duration; 5] = [
    Duration::from_millis(500),
    Duration::from_millis(500),
    Duration::from_millis(500),
    Duration::from_secs(8),
    Duration::from_secs(2),
];

pub(crate) trait TpccTransaction<S: Storage> {
    type Args;

    fn run(
        tx: &mut DBTransaction<S>,
        args: &Self::Args,
        statements: &[Statement],
    ) -> Result<(), TpccError>;
}

pub(crate) trait TpccTest<S: Storage> {
    fn name(&self) -> &'static str;

    fn do_transaction(
        &self,
        rng: &mut ThreadRng,
        tx: &mut DBTransaction<S>,
        num_ware: usize,
        args: &TpccArgs,
        statements: &[Statement],
    ) -> Result<(), TpccError>;
}

struct TpccArgs {
    joins: bool,
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[clap(long, default_value = "false")]
    joins: bool,
    #[clap(long, default_value = "kite_sql_tpcc")]
    path: String,
    #[clap(long, default_value = "5")]
    max_retry: usize,
    #[clap(long, default_value = "720")]
    measure_time: u64,
    #[clap(long, default_value = "1")]
    num_ware: usize,
}

// TODO: Support multi-threaded TPCC
fn main() -> Result<(), TpccError> {
    let args = Args::parse();

    let mut rng = rand::thread_rng();
    let database = DataBaseBuilder::path(&args.path).build()?;

    Load::load_items(&mut rng, &database)?;
    Load::load_warehouses(&mut rng, &database, args.num_ware)?;
    Load::load_custs(&mut rng, &database, args.num_ware)?;
    Load::load_ord(&mut rng, &database, args.num_ware)?;

    let test_statements = vec![
        vec![
            database.prepare("SELECT c.c_discount, c.c_last, c.c_credit, w.w_tax FROM customer AS c JOIN warehouse AS w ON c.c_w_id = w_id AND w.w_id = ?1 AND c.c_w_id = ?2 AND c.c_d_id = ?3 AND c.c_id = ?4")?,
            database.prepare("SELECT c_discount, c_last, c_credit FROM customer WHERE c_w_id = ?1 AND c_d_id = ?2 AND c_id = ?3")?,
            database.prepare("SELECT w_tax FROM warehouse WHERE w_id = ?1")?,
            database.prepare("SELECT d_next_o_id, d_tax FROM district WHERE d_id = ?1 AND d_w_id = ?2")?,
            database.prepare("UPDATE district SET d_next_o_id = ?1 + 1 WHERE d_id = ?2 AND d_w_id = ?3")?,
            database.prepare("INSERT INTO orders (o_id, o_d_id, o_w_id, o_c_id, o_entry_d, o_ol_cnt, o_all_local) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)")?,
            database.prepare("INSERT INTO new_orders (no_o_id, no_d_id, no_w_id) VALUES (?1,?2,?3)")?,
            database.prepare("SELECT i_price, i_name, i_data FROM item WHERE i_id = ?1")?,
            database.prepare("SELECT s_quantity, s_data, s_dist_01, s_dist_02, s_dist_03, s_dist_04, s_dist_05, s_dist_06, s_dist_07, s_dist_08, s_dist_09, s_dist_10 FROM stock WHERE s_i_id = ?1 AND s_w_id = ?2")?,
            database.prepare("UPDATE stock SET s_quantity = ?1 WHERE s_i_id = ?2 AND s_w_id = ?3")?,
            database.prepare("INSERT INTO order_line (ol_o_id, ol_d_id, ol_w_id, ol_number, ol_i_id, ol_supply_w_id, ol_quantity, ol_amount, ol_dist_info) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)")?,
        ],
        vec![
            database.prepare("UPDATE warehouse SET w_ytd = w_ytd + ?1 WHERE w_id = ?2")?,
            database.prepare("SELECT w_street_1, w_street_2, w_city, w_state, w_zip, w_name FROM warehouse WHERE w_id = ?1")?,
            database.prepare("UPDATE district SET d_ytd = d_ytd + ?1 WHERE d_w_id = ?2 AND d_id = ?3")?,
            database.prepare("SELECT d_street_1, d_street_2, d_city, d_state, d_zip, d_name FROM district WHERE d_w_id = ?1 AND d_id = ?2")?,
            database.prepare("SELECT count(c_id) FROM customer WHERE c_w_id = ?1 AND c_d_id = ?2 AND c_last = ?3")?,
            database.prepare("SELECT c_id FROM customer WHERE c_w_id = ?1 AND c_d_id = ?2 AND c_last = ?3 ORDER BY c_first")?,
            database.prepare("SELECT c_first, c_middle, c_last, c_street_1, c_street_2, c_city, c_state, c_zip, c_phone, c_credit, c_credit_lim, c_discount, c_balance, c_since FROM customer WHERE c_w_id = ?1 AND c_d_id = ?2 AND c_id = ?3")?,
            database.prepare("SELECT c_data FROM customer WHERE c_w_id = ?1 AND c_d_id = ?2 AND c_id = ?3")?,
            database.prepare("UPDATE customer SET c_balance = ?1, c_data = ?2 WHERE c_w_id = ?3 AND c_d_id = ?4 AND c_id = ?5")?,
            database.prepare("UPDATE customer SET c_balance = ?1 WHERE c_w_id = ?2 AND c_d_id = ?3 AND c_id = ?4")?,
            database.prepare("INSERT OVERWRITE history(h_c_d_id, h_c_w_id, h_c_id, h_d_id, h_w_id, h_date, h_amount, h_data) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)")?,
        ],
        vec![
            database.prepare("SELECT count(c_id) FROM customer WHERE c_w_id = ?1 AND c_d_id = ?2 AND c_last = ?3")?,
            database.prepare("SELECT c_balance, c_first, c_middle, c_last FROM customer WHERE c_w_id = ?1 AND c_d_id = ?2 AND c_last = ?3 ORDER BY c_first")?,
            database.prepare("SELECT c_balance, c_first, c_middle, c_last FROM customer WHERE c_w_id = ?1 AND c_d_id = ?2 AND c_id = ?3")?,
            database.prepare("SELECT o_id, o_entry_d, COALESCE(o_carrier_id,0) FROM orders WHERE o_w_id = ?1 AND o_d_id = ?2 AND o_c_id = ?3 AND o_id = (SELECT MAX(o_id) FROM orders WHERE o_w_id = ?4 AND o_d_id = ?5 AND o_c_id = ?6)")?,
            database.prepare("SELECT ol_i_id, ol_supply_w_id, ol_quantity, ol_amount, ol_delivery_d FROM order_line WHERE ol_w_id = ?1 AND ol_d_id = ?2 AND ol_o_id = ?3")?
        ],
        vec![
            database.prepare("SELECT COALESCE(MIN(no_o_id),0) FROM new_orders WHERE no_d_id = ?1 AND no_w_id = ?2")?,
            database.prepare("DELETE FROM new_orders WHERE no_o_id = ?1 AND no_d_id = ?2 AND no_w_id = ?3")?,
            database.prepare("SELECT o_c_id FROM orders WHERE o_id = ?1 AND o_d_id = ?2 AND o_w_id = ?3")?,
            database.prepare("UPDATE orders SET o_carrier_id = ?1 WHERE o_id = ?2 AND o_d_id = ?3 AND o_w_id = ?4")?,
            database.prepare("UPDATE order_line SET ol_delivery_d = ?1 WHERE ol_o_id = ?2 AND ol_d_id = ?3 AND ol_w_id = ?4")?,
            database.prepare("SELECT SUM(ol_amount) FROM order_line WHERE ol_o_id = ?1 AND ol_d_id = ?2 AND ol_w_id = ?3")?,
            database.prepare("UPDATE customer SET c_balance = c_balance + ?1 , c_delivery_cnt = c_delivery_cnt + 1 WHERE c_id = ?2 AND c_d_id = ?3 AND c_w_id = ?4")?,
        ],
        vec![
            database.prepare("SELECT d_next_o_id FROM district WHERE d_id = ?1 AND d_w_id = ?2")?,
            database.prepare("SELECT DISTINCT ol_i_id FROM order_line WHERE ol_w_id = ?1 AND ol_d_id = ?2 AND ol_o_id < ?3 AND ol_o_id >= (?4 - 20)")?,
            database.prepare("SELECT count(*) FROM stock WHERE s_w_id = ?1 AND s_i_id = ?2 AND s_quantity < ?3")?,
        ],
    ];

    let mut rt_hist = RtHist::new();
    let mut success = [0usize; 5];
    let mut late = [0usize; 5];
    let mut failure = [0usize; 5];
    let tests = vec![
        Box::new(NewOrdTest) as Box<dyn TpccTest<_>>,
        Box::new(PaymentTest),
        Box::new(OrderStatTest),
        Box::new(DeliveryTest),
        Box::new(SlevTest),
    ];
    let tpcc_args = TpccArgs { joins: args.joins };

    let duration = Duration::new(args.measure_time, 0);
    let mut round_count = 0;
    let mut seq_gen = SeqGen::new(10, 10, 1, 1, 1);
    let tpcc_start = Instant::now();

    while tpcc_start.elapsed() < duration {
        let i = seq_gen.get();
        let tpcc_test = &tests[i];
        let statement = &test_statements[i];

        let mut is_succeed = false;
        for j in 0..args.max_retry + 1 {
            let transaction_start = Instant::now();
            let mut tx = database.new_transaction()?;

            if let Err(err) =
                tpcc_test.do_transaction(&mut rng, &mut tx, args.num_ware, &tpcc_args, &statement)
            {
                failure[i] += 1;
                eprintln!(
                    "[{}] Error while doing transaction: {}",
                    tpcc_test.name(),
                    err
                );
            } else {
                let rt = transaction_start.elapsed();
                rt_hist.hist_inc(i, rt);
                is_succeed = true;

                if rt <= RT_LIMITS[i] {
                    success[i] += 1;
                } else {
                    late[i] += 1;
                }
                tx.commit()?;
                break;
            }
            if j < args.max_retry {
                println!("[{}] Retry for the {}th time", tpcc_test.name(), j + 1);
            }
        }
        if !is_succeed {
            return Err(TpccError::MaxRetry);
        }
        if round_count != 0 && round_count % 100 == 0 {
            println!(
                "[TPCC CheckPoint {} on round {round_count}][{}]: 90th Percentile RT: {:.3}",
                round_count / 100,
                tpcc_test.name(),
                rt_hist.hist_ckp(i)
            );
        }
        round_count += 1;
    }
    let actual_tpcc_time = tpcc_start.elapsed();
    println!("---------------------------------------------------");
    // Raw Results
    print_transaction(&success, &late, &failure, |name, success, late, failure| {
        println!("|{}| sc: {}  lt: {}  fl: {}", name, success, late, failure)
    });
    println!("in {} sec.", actual_tpcc_time.as_secs());
    println!("<Constraint Check> (all must be [OK])");
    println!("[transaction percentage]");

    let mut j = 0.0;
    for i in 0..5 {
        j += (success[i] + late[i]) as f64;
    }
    // Payment
    let f = (((success[1] + late[1]) as f64 / j) * 100.0).round();
    print!("   Payment: {:.1}% (>=43.0%)", f);
    if f >= 43.0 {
        println!("  [Ok]");
    } else {
        println!("  [NG]");
    }
    // Order-Status
    let f = (((success[2] + late[2]) as f64 / j) * 100.0).round();
    print!("   Order-Status: {:.1}% (>=4.0%)", f);
    if f >= 4.0 {
        println!("  [Ok]");
    } else {
        println!("  [NG]");
    }
    // Delivery
    let f = (((success[3] + late[3]) as f64 / j) * 100.0).round();
    print!("   Delivery: {:.1}% (>=4.0%)", f);
    if f >= 4.0 {
        println!("  [Ok]");
    } else {
        println!("  [NG]");
    }
    // Stock-Level
    let f = (((success[4] + late[4]) as f64 / j) * 100.0).round();
    print!("   Stock-Level: {:.1}% (>=4.0%)", f);
    if f >= 4.0 {
        println!("  [Ok]");
    } else {
        println!("  [NG]");
    }
    println!("[response time (at least 90%% passed)]");
    print_transaction(&success, &late, &failure, |name, success, late, _| {
        let f = (success as f64 / (success + late) as f64) * 100.0;
        print!("   {}: {:.1}", name, f);
        if f >= 90.0 {
            println!("  [OK]");
        } else {
            println!("  [NG]");
        }
    });
    print_transaction(&success, &late, &failure, |name, success, late, _| {
        println!("   {} Total: {}", name, success + late)
    });
    println!();
    rt_hist.hist_report();
    println!("<TpmC>");
    let tpmc = ((success[0] + late[0]) as f64 / (actual_tpcc_time.as_secs_f64() / 60.0)).round();
    println!("{} Tpmc", tpmc);

    Ok(())
}

fn print_transaction<F: Fn(&str, usize, usize, usize)>(
    success: &[usize],
    late: &[usize],
    failure: &[usize],
    fn_print: F,
) {
    for (i, name) in vec![
        "New-Order",
        "Payment",
        "Order-Status",
        "Delivery",
        "Stock-Level",
    ]
    .into_iter()
    .enumerate()
    {
        fn_print(name, success[i], late[i], failure[i]);
    }
}

fn other_ware(rng: &mut ThreadRng, home_ware: usize, num_ware: usize) -> usize {
    if num_ware == 1 {
        return home_ware;
    }

    loop {
        let tmp = rng.gen_range(1..num_ware);
        if tmp != home_ware {
            return tmp;
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TpccError {
    #[error("kite_sql: {0}")]
    Database(
        #[source]
        #[from]
        DatabaseError,
    ),
    #[error("tuples is empty")]
    EmptyTuples,
    #[error("maximum retries reached")]
    MaxRetry,
}

#[ignore]
#[test]
fn explain_tpcc() -> Result<(), DatabaseError> {
    use kite_sql::types::tuple::create_table;

    let database = DataBaseBuilder::path("./kite_sql_tpcc").build()?;
    let mut tx = database.new_transaction()?;

    let customer_tuple = tx
        .run("SELECT c_w_id, c_d_id, c_id, c_last, c_balance, c_data FROM customer limit 1")?
        .next()
        .unwrap()?;
    let district_tuple = tx
        .run("SELECT d_id, d_w_id, d_next_o_id FROM district limit 1")?
        .next()
        .unwrap()?;
    let item_tuple = tx.run("SELECT i_id FROM item limit 1")?.next().unwrap()?;
    let stock_tuple = tx
        .run("SELECT s_i_id, s_w_id, s_quantity FROM stock limit 1")?
        .next()
        .unwrap()?;
    let orders_tuple = tx
        .run("SELECT o_w_id, o_d_id, o_c_id, o_id, o_carrier_id FROM orders limit 1")?
        .next()
        .unwrap()?;
    let order_line_tuple = tx
        .run("SELECT ol_w_id, ol_d_id, ol_o_id, ol_delivery_d FROM order_line limit 1")?
        .next()
        .unwrap()?;
    let new_order_tuple = tx
        .run("SELECT no_d_id, no_w_id, no_o_id FROM new_orders limit 1")?
        .next()
        .unwrap()?;

    let c_w_id = customer_tuple.values[0].clone();
    let c_d_id = customer_tuple.values[1].clone();
    let c_id = customer_tuple.values[2].clone();
    let c_last = customer_tuple.values[3].clone();
    let c_balance = customer_tuple.values[4].clone();
    let c_data = customer_tuple.values[5].clone();

    let d_id = district_tuple.values[0].clone();
    let d_w_id = district_tuple.values[1].clone();
    let d_next_o_id = district_tuple.values[2].clone();

    let i_id = item_tuple.values[0].clone();

    let s_i_id = stock_tuple.values[0].clone();
    let s_w_id = stock_tuple.values[1].clone();
    let s_quantity = stock_tuple.values[2].clone();

    let o_w_id = orders_tuple.values[0].clone();
    let o_d_id = orders_tuple.values[1].clone();
    let o_c_id = orders_tuple.values[2].clone();
    let o_id = orders_tuple.values[3].clone();
    let o_carrier_id = orders_tuple.values[4].clone();

    let ol_w_id = order_line_tuple.values[0].clone();
    let ol_d_id = order_line_tuple.values[1].clone();
    let ol_o_id = order_line_tuple.values[2].clone();
    let ol_delivery_d = order_line_tuple.values[3].clone();

    let no_d_id = new_order_tuple.values[0].clone();
    let no_w_id = new_order_tuple.values[1].clone();
    let no_o_id = new_order_tuple.values[2].clone();
    // ORDER
    {
        println!("========Explain on Order");
        {
            println!("{}", format!("explain SELECT c_discount, c_last, c_credit FROM customer WHERE c_w_id = {} AND c_d_id = {} AND c_id = {}", c_w_id, c_d_id, c_id));
            let iter = tx.run(format!("explain SELECT c_discount, c_last, c_credit FROM customer WHERE c_w_id = {} AND c_d_id = {} AND c_id = {}", c_w_id, c_d_id, c_id))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!(
                "explain SELECT d_next_o_id, d_tax FROM district WHERE d_id = {} AND d_w_id = {}",
                d_id, d_w_id
            ))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!(
                "explain UPDATE district SET d_next_o_id = {} + 1 WHERE d_id = {} AND d_w_id = {}",
                d_next_o_id, d_id, d_w_id
            ))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!(
                "explain SELECT i_price, i_name, i_data FROM item WHERE i_id = {}",
                i_id
            ))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain SELECT s_quantity, s_data, s_dist_01, s_dist_02, s_dist_03, s_dist_04, s_dist_05, s_dist_06, s_dist_07, s_dist_08, s_dist_09, s_dist_10 FROM stock WHERE s_i_id = {} AND s_w_id = {}", s_i_id, s_w_id))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!(
                "explain UPDATE stock SET s_quantity = {} WHERE s_i_id = {} AND s_w_id = {}",
                s_quantity, s_i_id, s_w_id
            ))?;

            println!("{}", create_table(iter)?);
        }
    }

    // Payment
    {
        println!("========Explain on Payment");
        {
            let iter = tx.run(format!(
                "explain UPDATE stock SET s_quantity = {} WHERE s_i_id = {} AND s_w_id = {}",
                s_quantity, s_i_id, s_w_id
            ))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain SELECT d_street_1, d_street_2, d_city, d_state, d_zip, d_name FROM district WHERE d_w_id = {} AND d_id = {}", d_w_id, d_id))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain SELECT count(c_id) FROM customer WHERE c_w_id = {} AND c_d_id = {} AND c_last = '{}'", c_w_id, c_d_id, c_last))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain SELECT c_id FROM customer WHERE c_w_id = {} AND c_d_id = {} AND c_last = '{}' ORDER BY c_first", c_w_id, c_d_id, c_last))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain SELECT c_first, c_middle, c_last, c_street_1, c_street_2, c_city, c_state, c_zip, c_phone, c_credit, c_credit_lim, c_discount, c_balance, c_since FROM customer WHERE c_w_id = {} AND c_d_id = {} AND c_id = {}", c_w_id, c_d_id, c_id))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain SELECT c_data FROM customer WHERE c_w_id = {} AND c_d_id = {} AND c_id = {}", c_w_id, c_d_id, c_id))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain UPDATE customer SET c_balance = {}, c_data = '{}' WHERE c_w_id = {} AND c_d_id = {} AND c_id = {}", c_balance, c_data, c_w_id, c_d_id, c_id))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain UPDATE customer SET c_balance = {} WHERE c_w_id = {} AND c_d_id = {} AND c_id = {}", c_balance, c_w_id, c_d_id, c_id))?;

            println!("{}", create_table(iter)?);
        }
    }

    // Order-Stat
    {
        println!("========Explain on Order-Stat");
        {
            let iter = tx.run(format!("explain SELECT count(c_id) FROM customer WHERE c_w_id = {} AND c_d_id = {} AND c_last = '{}'", c_w_id, c_d_id, c_last))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain SELECT c_balance, c_first, c_middle, c_last FROM customer WHERE c_w_id = {} AND c_d_id = {} AND c_last = '{}' ORDER BY c_first", c_w_id, c_d_id, c_last))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain SELECT c_balance, c_first, c_middle, c_last FROM customer WHERE c_w_id = {} AND c_d_id = {} AND c_id = {}", c_w_id, c_d_id, c_id))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain SELECT o_id, o_entry_d, COALESCE(o_carrier_id,0) FROM orders WHERE o_w_id = {} AND o_d_id = {} AND o_c_id = {} AND o_id = (SELECT MAX(o_id) FROM orders WHERE o_w_id = {} AND o_d_id = {} AND o_c_id = {})", o_w_id, o_d_id, o_c_id, o_w_id, o_d_id, o_c_id))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain SELECT ol_i_id, ol_supply_w_id, ol_quantity, ol_amount, ol_delivery_d FROM order_line WHERE ol_w_id = {} AND ol_d_id = {} AND ol_o_id = {}", ol_w_id, ol_d_id, ol_o_id))?;

            println!("{}", create_table(iter)?);
        }
    }

    // Deliver
    {
        println!("========Explain on Deliver");
        {
            let iter = tx.run(format!("explain SELECT COALESCE(MIN(no_o_id),0) FROM new_orders WHERE no_d_id = {} AND no_w_id = {}", no_d_id, no_w_id))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain DELETE FROM new_orders WHERE no_o_id = {} AND no_d_id = {} AND no_w_id = {}", no_o_id, no_d_id, no_w_id))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!(
                "explain SELECT o_c_id FROM orders WHERE o_id = {} AND o_d_id = {} AND o_w_id = {}",
                o_id, o_d_id, o_w_id
            ))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain UPDATE orders SET o_carrier_id = {} WHERE o_id = {} AND o_d_id = {} AND o_w_id = {}", o_carrier_id, o_id, o_d_id, o_w_id))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain UPDATE order_line SET ol_delivery_d = '{}' WHERE ol_o_id = {} AND ol_d_id = {} AND ol_w_id = {}", ol_delivery_d, ol_o_id, ol_d_id, ol_w_id))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain SELECT SUM(ol_amount) FROM order_line WHERE ol_o_id = {} AND ol_d_id = {} AND ol_w_id = {}", ol_o_id, ol_d_id, ol_w_id))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain UPDATE customer SET c_balance = c_balance + 1 , c_delivery_cnt = c_delivery_cnt + 1 WHERE c_id = {} AND c_d_id = {} AND c_w_id = {}", c_id, c_d_id, c_w_id))?;

            println!("{}", create_table(iter)?);
        }
    }

    // Stock-Level
    {
        println!("========Explain on Stock-Level");
        {
            let iter = tx.run(format!(
                "explain SELECT d_next_o_id FROM district WHERE d_id = {} AND d_w_id = {}",
                d_id, d_w_id
            ))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain SELECT DISTINCT ol_i_id FROM order_line WHERE ol_w_id = {} AND ol_d_id = {} AND ol_o_id < {} AND ol_o_id >= ({} - 20)", ol_w_id, ol_d_id, ol_o_id, ol_o_id))?;

            println!("{}", create_table(iter)?);
        }
        {
            let iter = tx.run(format!("explain SELECT count(*) FROM stock WHERE s_w_id = {} AND s_i_id = {} AND s_quantity < {}", s_w_id, s_i_id, s_quantity))?;

            println!("{}", create_table(iter)?);
        }
    }

    Ok(())
}
